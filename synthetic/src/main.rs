//! Ground-truth reproducer for whyslow's integration test.
//!
//! Two threads, one deterministic causal chain:
//!   - "holder" acquires a raw futex-based lock, then does a cold, O_DIRECT
//!     (page-cache-bypassing) read from disk -- a real block I/O stall.
//!   - "waiter" contends for the same lock while holder is still doing the
//!     read, so it genuinely blocks in the kernel (`futex(FUTEX_WAIT)`) until
//!     holder's `unlock()` wakes it.
//!
//! Ground truth: waiter's futex wait is caused by holder's wake, which is
//! caused by holder's block I/O. `whyslow explain` should reproduce exactly
//! that chain.
//!
//! We hand-roll the lock with raw `futex(2)` syscalls (Drepper's classic
//! 3-state mutex, simplified) instead of `std::sync::Mutex` so the exact futex
//! ops observed are under our control and don't depend on libc/std internals.

use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

// Earlier version wrote its own scratch file + `fsync()`'d it before spawning
// any threads, to guarantee data was durably on disk before the O_DIRECT
// read. That write+fsync ran on the *main* thread, which is itself real block
// I/O -- it showed up in whyslow's own trace as a competing (and sometimes
// longer) stall attributed to the main thread, burying the actual
// holder/waiter chain under test. `O_DIRECT` bypasses the page cache
// unconditionally regardless of whether the data is already cache-resident,
// so there's no need to write anything at all: reading a chunk of our own
// already-on-disk executable via `O_DIRECT` is just as real a block I/O stall,
// with zero incidental I/O anywhere else in the traced process.

const FILE_SIZE: usize = 64 * 1024; // comfortably under typical max_sectors_kb,
// so each individual read stays a single block_rq_issue/complete pair.
const ALIGN: usize = 4096;
// Repeat the read this many times so holder's total I/O phase reliably
// outlasts waiter's thread-spawn/scheduling latency -- a single 64KiB NVMe
// read can complete in under the time it takes the OS to schedule a brand
// new thread for the first time, which would let waiter's lock() attempt
// arrive *after* holder already unlocked, never actually contending (and
// so never producing the futex_wait we need for the test). Repeating keeps
// each request small (still one block_rq pair each) while making the whole
// holding period long enough to be robust to scheduling jitter.
const READ_REPEATS: usize = 64;

fn gettid() -> i32 {
    unsafe { libc::syscall(libc::SYS_gettid) as i32 }
}

fn futex_wait(word: &AtomicU32, expected: u32) {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word as *const AtomicU32 as *const u32,
            libc::FUTEX_WAIT,
            expected,
            std::ptr::null::<libc::timespec>(),
        );
    }
}

fn futex_wake(word: &AtomicU32, n: i32) {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word as *const AtomicU32 as *const u32,
            libc::FUTEX_WAKE,
            n,
        );
    }
}

/// Drepper's classic 3-state futex mutex (0=unlocked, 1/2=locked), simplified:
/// every contended path sets state 2 rather than trying 1 first. Correct for
/// mutual exclusion; not the fastest possible futex mutex, which we don't need.
fn lock(word: &AtomicU32) {
    if word
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire)
        .is_ok()
    {
        return;
    }
    loop {
        if word.swap(2, Ordering::Acquire) == 0 {
            return;
        }
        futex_wait(word, 2);
    }
}

fn unlock(word: &AtomicU32) {
    if word.swap(0, Ordering::Release) == 2 {
        futex_wake(word, 1);
    }
}

struct AlignedBuf {
    ptr: *mut u8,
    layout: std::alloc::Layout,
}

impl AlignedBuf {
    fn new(size: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(size, ALIGN).unwrap();
        let ptr = unsafe { std::alloc::alloc(layout) };
        assert!(!ptr.is_null());
        Self { ptr, layout }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

fn cold_direct_read(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)?;
    let mut buf = AlignedBuf::new(FILE_SIZE);
    for _ in 0..READ_REPEATS {
        let n = unsafe {
            libc::pread(
                file.as_raw_fd(),
                buf.as_mut_slice().as_mut_ptr().cast(),
                FILE_SIZE,
                0,
            )
        };
        if n < 0 {
            return Err(anyhow::anyhow!(
                "O_DIRECT pread failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let read_source = std::env::current_exe()?;
    assert!(
        std::fs::metadata(&read_source)?.len() as usize >= FILE_SIZE,
        "own executable unexpectedly smaller than {FILE_SIZE} bytes"
    );

    let word = Arc::new(AtomicU32::new(0));
    let holder_acquired = Arc::new(AtomicBool::new(false));
    let holder_tid = Arc::new(AtomicU32::new(0));
    let waiter_tid = Arc::new(AtomicU32::new(0));

    let holder = std::thread::spawn({
        let word = word.clone();
        let holder_acquired = holder_acquired.clone();
        let holder_tid = holder_tid.clone();
        move || {
            holder_tid.store(gettid() as u32, Ordering::SeqCst);
            lock(&word);
            holder_acquired.store(true, Ordering::SeqCst);
            cold_direct_read(&read_source).expect("cold_direct_read");
            unlock(&word);
        }
    });

    // Wait for holder to actually hold the lock (and be about to start the
    // slow read) before letting waiter contend for it, so waiter's futex_wait
    // is guaranteed real, not a race. This flag isn't itself futex-based, so
    // it's invisible to whyslow's tracer.
    while !holder_acquired.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_micros(100));
    }

    let waiter = std::thread::spawn({
        let word = word.clone();
        let waiter_tid = waiter_tid.clone();
        move || {
            waiter_tid.store(gettid() as u32, Ordering::SeqCst);
            lock(&word);
            unlock(&word);
        }
    });

    holder.join().expect("holder thread panicked");
    waiter.join().expect("waiter thread panicked");

    println!(
        "SYNTHETIC_DONE pid={} holder_tid={} waiter_tid={}",
        std::process::id(),
        holder_tid.load(Ordering::SeqCst),
        waiter_tid.load(Ordering::SeqCst)
    );
    std::io::stdout().flush().ok();
    Ok(())
}
