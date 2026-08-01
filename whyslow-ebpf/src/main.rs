#![no_std]
#![no_main]

mod vmlinux;

use aya_ebpf::{
    helpers::{bpf_get_current_comm, bpf_get_current_pid_tgid, bpf_ktime_get_ns},
    macros::{map, tracepoint},
    maps::{Array, HashMap, RingBuf},
    programs::TracePointContext,
};
use vmlinux::{
    FUTEX_CMD_MASK, FUTEX_WAIT, FUTEX_WAIT_BITSET, FUTEX_WAKE, FUTEX_WAKE_BITSET, TASK_RUNNING,
    TraceEventRawBlockRq, TraceEventRawBlockRqCompletion, TraceEventRawSchedSwitch,
    TraceEventRawSysEnter,
};
use whyslow_common::{EVENT_BLOCK_IO, EVENT_FUTEX_WAIT, EVENT_FUTEX_WAKE, EVENT_SCHED_BLOCK, Event, pack_block_key};

/// Single shared ring buffer for every event type (per the "one map, fixed-size
/// structs, no per-event heap alloc" constraint). 256 KiB is generous for a
/// short-lived trace of one process tree; sized in whole pages, power-of-2 per
/// kernel ring buffer requirements.
#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

/// The tgid whyslow is tracing. 0 means "not set yet" -- every probe drops
/// events in that state, so there's no window where we accidentally trace the
/// whole system. Single-slot array map, set by userspace right after load.
#[map]
static TARGET_TGID: Array<u32> = Array::with_max_entries(1, 0);

#[derive(Copy, Clone)]
#[repr(C)]
struct BlockStart {
    ts: u64,
    tgid: u32,
    comm: [u8; 16],
}

/// Keyed by tid: when a traced thread goes off-CPU in a non-runnable state, we
/// stash when/who here; when that same tid next appears as the *incoming*
/// thread of a sched_switch, we compute the elapsed time in one step. This
/// coalesces what would otherwise be two raw switch events into one
/// ready-made "blocked for duration_ns" record, halving ring buffer traffic
/// for this source.
#[map]
static BLOCK_START: HashMap<u32, BlockStart> = HashMap::with_max_entries(10240, 0);

#[derive(Copy, Clone)]
#[repr(C)]
struct FutexStart {
    ts: u64,
    uaddr: u64,
    op: u32,
    tgid: u32,
    comm: [u8; 16],
}

/// Keyed by tid: futex syscall args captured at enter, consumed at exit (only
/// sys_exit carries the return value / completion timestamp we need).
#[map]
static FUTEX_START: HashMap<u32, FutexStart> = HashMap::with_max_entries(10240, 0);

#[derive(Copy, Clone)]
#[repr(C)]
struct BlockIoStart {
    ts: u64,
    tgid: u32,
    tid: u32,
    comm: [u8; 16],
}

/// Keyed by packed (dev, sector): block_rq_complete fires in interrupt/softirq
/// context, not the submitting thread's, so we must remember who issued the
/// request rather than reading "current" at completion time.
#[map]
static BLOCKIO_START: HashMap<u64, BlockIoStart> = HashMap::with_max_entries(4096, 0);

#[inline(always)]
fn target_tgid() -> u32 {
    TARGET_TGID.get(0).copied().unwrap_or(0)
}

#[inline(always)]
fn current_tgid() -> u32 {
    (bpf_get_current_pid_tgid() >> 32) as u32
}

#[inline(always)]
fn current_tid() -> u32 {
    (bpf_get_current_pid_tgid() & 0xFFFF_FFFF) as u32
}

#[inline(always)]
fn emit(ev: Event) {
    if let Some(mut entry) = EVENTS.reserve::<Event>(0) {
        entry.write(ev);
        entry.submit(0);
    }
    // If the ring buffer is full we drop the event rather than block/spin --
    // this program must not be able to stall the thing it's measuring.
}

#[tracepoint]
pub fn sched_switch(ctx: TracePointContext) -> u32 {
    match try_sched_switch(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_sched_switch(ctx: TracePointContext) -> Result<u32, u32> {
    let target = target_tgid();
    if target == 0 {
        return Ok(0);
    }

    let evt: TraceEventRawSchedSwitch = unsafe { ctx.read_at(0) }.map_err(|_| 1u32)?;
    let ts = unsafe { bpf_ktime_get_ns() };
    let prev_tid = evt.prev_pid as u32;
    let next_tid = evt.next_pid as u32;

    if evt.prev_state != TASK_RUNNING {
        // sched_switch runs in the outgoing (prev) task's context, so its own
        // tgid/comm are still valid to read here.
        let prev_tgid = current_tgid();
        if prev_tgid == target {
            let start = BlockStart {
                ts,
                tgid: prev_tgid,
                comm: evt.prev_comm,
            };
            let _ = BLOCK_START.insert(prev_tid, start, 0);
        }
    }

    if let Some(start) = unsafe { BLOCK_START.get(&next_tid) }.copied() {
        let _ = BLOCK_START.remove(&next_tid);
        let duration = ts.saturating_sub(start.ts);
        emit(Event::new(
            ts,
            duration,
            0,
            start.tgid,
            next_tid,
            EVENT_SCHED_BLOCK,
            start.comm,
        ));
    }

    Ok(0)
}

#[tracepoint]
pub fn futex_enter(ctx: TracePointContext) -> u32 {
    match try_futex_enter(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_futex_enter(ctx: TracePointContext) -> Result<u32, u32> {
    let target = target_tgid();
    let tgid = current_tgid();
    if target == 0 || tgid != target {
        return Ok(0);
    }

    let evt: TraceEventRawSysEnter = unsafe { ctx.read_at(0) }.map_err(|_| 1u32)?;
    let uaddr = evt.args[0];
    let op = evt.args[1] as u32;
    let tid = current_tid();
    let comm = bpf_get_current_comm().unwrap_or([0; 16]);
    let ts = unsafe { bpf_ktime_get_ns() };

    let start = FutexStart {
        ts,
        uaddr,
        op,
        tgid,
        comm,
    };
    let _ = FUTEX_START.insert(tid, start, 0);
    Ok(0)
}

#[tracepoint]
pub fn futex_exit(ctx: TracePointContext) -> u32 {
    match try_futex_exit(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_futex_exit(_ctx: TracePointContext) -> Result<u32, u32> {
    let tid = current_tid();
    let start = match unsafe { FUTEX_START.get(&tid) }.copied() {
        Some(s) => s,
        None => return Ok(0),
    };
    let _ = FUTEX_START.remove(&tid);

    let ts = unsafe { bpf_ktime_get_ns() };
    let duration = ts.saturating_sub(start.ts);
    let base_op = start.op & FUTEX_CMD_MASK;

    let event_type = if base_op == FUTEX_WAIT || base_op == FUTEX_WAIT_BITSET {
        EVENT_FUTEX_WAIT
    } else if base_op == FUTEX_WAKE || base_op == FUTEX_WAKE_BITSET {
        EVENT_FUTEX_WAKE
    } else {
        return Ok(0); // other futex ops (LOCK_PI, REQUEUE, ...) unhandled in v1
    };

    emit(Event::new(
        ts,
        duration,
        start.uaddr,
        start.tgid,
        tid,
        event_type,
        start.comm,
    ));
    Ok(0)
}

#[tracepoint]
pub fn block_rq_issue(ctx: TracePointContext) -> u32 {
    match try_block_rq_issue(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_block_rq_issue(ctx: TracePointContext) -> Result<u32, u32> {
    let target = target_tgid();
    let tgid = current_tgid();
    if target == 0 || tgid != target {
        return Ok(0);
    }

    let evt: TraceEventRawBlockRq = unsafe { ctx.read_at(0) }.map_err(|_| 1u32)?;
    let tid = current_tid();
    let comm = bpf_get_current_comm().unwrap_or([0; 16]);
    let ts = unsafe { bpf_ktime_get_ns() };
    let key = pack_block_key(evt.dev, evt.sector);

    let start = BlockIoStart {
        ts,
        tgid,
        tid,
        comm,
    };
    let _ = BLOCKIO_START.insert(key, start, 0);
    Ok(0)
}

#[tracepoint]
pub fn block_rq_complete(ctx: TracePointContext) -> u32 {
    match try_block_rq_complete(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_block_rq_complete(ctx: TracePointContext) -> Result<u32, u32> {
    // Deliberately not filtered by current tgid: block_rq_complete typically
    // fires in interrupt/softirq context, not the submitting thread's -- the
    // identity we want was already captured (and filtered) at issue time.
    let evt: TraceEventRawBlockRqCompletion = unsafe { ctx.read_at(0) }.map_err(|_| 1u32)?;
    let key = pack_block_key(evt.dev, evt.sector);

    let start = match unsafe { BLOCKIO_START.get(&key) }.copied() {
        Some(s) => s,
        None => return Ok(0),
    };
    let _ = BLOCKIO_START.remove(&key);

    let ts = unsafe { bpf_ktime_get_ns() };
    let duration = ts.saturating_sub(start.ts);
    emit(Event::new(
        ts,
        duration,
        key,
        start.tgid,
        start.tid,
        EVENT_BLOCK_IO,
        start.comm,
    ));
    Ok(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
