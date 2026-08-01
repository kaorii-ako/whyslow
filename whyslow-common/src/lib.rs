#![no_std]

//! Event schema shared between the `whyslow-ebpf` kernel-side programs and the
//! `whyslow-cli` userspace correlation engine. Kept `no_std` and dependency-free
//! (aside from the optional `aya::Pod` impl) so it compiles unmodified into the
//! BPF target.

/// Thread went off-CPU (blocked, non-runnable) and later resumed running.
/// `duration_ns` is the time spent off-CPU; `key` is unused (0).
pub const EVENT_SCHED_BLOCK: u8 = 0;
/// A `futex(FUTEX_WAIT[_BITSET])` syscall returned. `duration_ns` is the full
/// syscall span (enter -> exit); `key` is the futex address (`uaddr`).
pub const EVENT_FUTEX_WAIT: u8 = 1;
/// A `futex(FUTEX_WAKE[_BITSET])` syscall returned. `duration_ns` is the
/// syscall's own (usually tiny) span; `key` is the futex address (`uaddr`).
pub const EVENT_FUTEX_WAKE: u8 = 2;
/// A block I/O request this thread issued has completed. `duration_ns` is the
/// issue -> complete latency; `key` packs `(dev, sector)`, see [`pack_block_key`].
pub const EVENT_BLOCK_IO: u8 = 3;

pub const TASK_COMM_LEN: usize = 16;

/// Fixed-size event record written to the shared BPF ring buffer.
///
/// No event type uses every field; unused fields are zeroed. This keeps a
/// single record type / single ring buffer map, per the no-per-event-heap-alloc
/// constraint, at the cost of a few unused bytes on some variants.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct Event {
    /// Timestamp (CLOCK_MONOTONIC / boot-time ns, i.e. `bpf_ktime_get_ns()`) at
    /// which this event became knowable, i.e. the *end* of whatever span it
    /// describes (thread resumed running / syscall returned / I/O completed).
    pub timestamp_ns: u64,
    /// Length of the span this event describes (see `EVENT_*` docs above).
    pub duration_ns: u64,
    /// Event-specific key: futex address, or a packed (dev, sector) for block I/O.
    pub key: u64,
    /// Thread group ID (what userspace calls "pid").
    pub pid: u32,
    /// Thread ID.
    pub tid: u32,
    pub event_type: u8,
    _pad: [u8; 3],
    pub comm: [u8; TASK_COMM_LEN],
}

impl Event {
    pub fn new(
        timestamp_ns: u64,
        duration_ns: u64,
        key: u64,
        pid: u32,
        tid: u32,
        event_type: u8,
        comm: [u8; TASK_COMM_LEN],
    ) -> Self {
        Self {
            timestamp_ns,
            duration_ns,
            key,
            pid,
            tid,
            event_type,
            _pad: [0; 3],
            comm,
        }
    }
}

#[cfg(feature = "user")]
unsafe impl aya::Pod for Event {}

/// Pack a block device's `(major:minor, sector)` into the 64-bit `Event::key`.
///
/// v1 simplification: `sector` is truncated to its low 32 bits. `dev_t` itself
/// is already a 32-bit kernel value on Linux, so `dev` is carried in full. This
/// covers realistic single-device sector ranges (2^32 sectors == 2TiB at a
/// 512-byte logical sector size, larger still for 4Kn devices); documented as a
/// known limitation for pathologically large devices in README.md.
#[inline]
pub fn pack_block_key(dev: u32, sector: u64) -> u64 {
    ((dev as u64) << 32) | (sector & 0xFFFF_FFFF)
}

#[inline]
pub fn unpack_block_key(key: u64) -> (u32, u32) {
    ((key >> 32) as u32, (key & 0xFFFF_FFFF) as u32)
}
