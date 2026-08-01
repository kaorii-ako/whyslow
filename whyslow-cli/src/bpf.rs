use std::os::fd::AsRawFd;

use anyhow::{Context, anyhow};
use aya::programs::TracePoint;

use crate::trace::TraceEvent;

/// (BPF program name, tracepoint category, tracepoint name) for every probe
/// whyslow-ebpf defines. Kept as one list so load/attach order is obvious and
/// there's exactly one place to add a future event source.
const TRACEPOINTS: &[(&str, &str, &str)] = &[
    ("sched_switch", "sched", "sched_switch"),
    ("futex_enter", "syscalls", "sys_enter_futex"),
    ("futex_exit", "syscalls", "sys_exit_futex"),
    ("block_rq_issue", "block", "block_rq_issue"),
    ("block_rq_complete", "block", "block_rq_complete"),
];

fn bump_memlock_rlimit() {
    // Needed on older kernels that don't use memcg-based accounting for BPF
    // map memory; see https://lwn.net/Articles/837122/. Harmless on newer ones.
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };
}

/// Load whyslow-ebpf and attach every tracepoint. The target tgid filter is
/// left unset (0, "trace nothing") until [`set_target_tgid`] is called -- no
/// probe will match anything real before that.
pub fn load_and_attach() -> anyhow::Result<aya::Ebpf> {
    bump_memlock_rlimit();

    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/whyslow-ebpf"
    )))
    .context("loading whyslow-ebpf object")?;

    for (prog_name, category, tp_name) in TRACEPOINTS {
        let program: &mut TracePoint = ebpf
            .program_mut(prog_name)
            .ok_or_else(|| anyhow!("BPF program `{prog_name}` missing from object"))?
            .try_into()?;
        program
            .load()
            .with_context(|| format!("loading BPF program `{prog_name}`"))?;
        program
            .attach(category, tp_name)
            .with_context(|| format!("attaching to tracepoint {category}:{tp_name}"))?;
    }

    Ok(ebpf)
}

/// Set the tgid whyslow traces. Every probe drops events until this is called,
/// so callers using the stop-before-exec dance (see `child.rs`) can load and
/// attach first, then set the target, then resume the child with no window
/// where unrelated system activity could be captured.
pub fn set_target_tgid(ebpf: &mut aya::Ebpf, tgid: u32) -> anyhow::Result<()> {
    let mut map: aya::maps::Array<_, u32> = aya::maps::Array::try_from(
        ebpf.map_mut("TARGET_TGID")
            .ok_or_else(|| anyhow!("TARGET_TGID map missing from object"))?,
    )?;
    map.set(0, tgid, 0)?;
    Ok(())
}

/// Drain the shared ring buffer until `should_stop` returns true, polling with
/// a bounded timeout so `should_stop` (e.g. "has the child exited?") gets
/// re-checked regularly even with no event traffic.
pub fn drain_ring_buffer(
    ebpf: &mut aya::Ebpf,
    mut should_stop: impl FnMut() -> bool,
) -> anyhow::Result<Vec<TraceEvent>> {
    let mut ring: aya::maps::RingBuf<_> = aya::maps::RingBuf::try_from(
        ebpf.map_mut("EVENTS")
            .ok_or_else(|| anyhow!("EVENTS map missing from object"))?,
    )?;
    let raw_fd = ring.as_raw_fd();
    let mut events = Vec::new();

    loop {
        while let Some(item) = ring.next() {
            if let Some(ev) = decode_event(&item) {
                events.push(TraceEvent::from(ev));
            }
        }
        if should_stop() {
            // One last drain: events may have landed between the check above
            // and the caller deciding to stop.
            while let Some(item) = ring.next() {
                if let Some(ev) = decode_event(&item) {
                    events.push(TraceEvent::from(ev));
                }
            }
            break;
        }
        let mut pfd = libc::pollfd {
            fd: raw_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pfd, 1, 100) }; // 100ms: bounds should_stop staleness
    }

    Ok(events)
}

fn decode_event(item: &[u8]) -> Option<whyslow_common::Event> {
    if item.len() != std::mem::size_of::<whyslow_common::Event>() {
        return None;
    }
    let mut uninit = std::mem::MaybeUninit::<whyslow_common::Event>::uninit();
    unsafe {
        std::ptr::copy_nonoverlapping(item.as_ptr(), uninit.as_mut_ptr().cast::<u8>(), item.len());
        Some(uninit.assume_init())
    }
}
