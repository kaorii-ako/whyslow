//! Happens-before edge inference and the `explain` chain-walking algorithm.
//!
//! See README.md "How happens-before edges are inferred" for the design
//! rationale; this file is the literal implementation of that design.

use std::collections::HashMap;

use crate::trace::{TraceEvent, TraceFile};
use whyslow_common::{EVENT_BLOCK_IO, EVENT_FUTEX_WAIT, EVENT_FUTEX_WAKE, EVENT_SCHED_BLOCK, unpack_block_key};

/// Matching tolerance for every "nearest preceding X within a bounded window"
/// join in this file (wake -> wait, block interval -> its futex/block-io
/// explanation, waker's own resume -> its own root cause). Confirmed design
/// choice: bounded rather than unbounded, to avoid matching a stale event from
/// an earlier, unrelated contention cycle.
const MATCH_WINDOW_NS: u64 = 5_000_000; // 5ms

const MAX_CHAIN_DEPTH: u32 = 8;

struct Typed<'a> {
    block: Vec<&'a TraceEvent>,
    futex_wait: Vec<&'a TraceEvent>,
    futex_wake: Vec<&'a TraceEvent>,
    block_io: Vec<&'a TraceEvent>,
}

impl<'a> Typed<'a> {
    fn partition(events: &'a [TraceEvent]) -> Self {
        let mut t = Typed {
            block: Vec::new(),
            futex_wait: Vec::new(),
            futex_wake: Vec::new(),
            block_io: Vec::new(),
        };
        for e in events {
            match e.event_type {
                EVENT_SCHED_BLOCK => t.block.push(e),
                EVENT_FUTEX_WAIT => t.futex_wait.push(e),
                EVENT_FUTEX_WAKE => t.futex_wake.push(e),
                EVENT_BLOCK_IO => t.block_io.push(e),
                _ => {}
            }
        }
        t
    }
}

/// Among `candidates` matching `pred`, the one whose `timestamp_ns` (an end
/// time -- every event in this schema is stamped at completion) is the latest
/// one at or before `anchor_ts`, but no more than `MATCH_WINDOW_NS` earlier.
fn nearest_preceding<'a>(
    candidates: &[&'a TraceEvent],
    anchor_ts: u64,
    pred: impl Fn(&TraceEvent) -> bool,
) -> Option<&'a TraceEvent> {
    candidates
        .iter()
        .copied()
        .filter(|e| pred(e) && e.timestamp_ns <= anchor_ts && anchor_ts - e.timestamp_ns <= MATCH_WINDOW_NS)
        .max_by_key(|e| e.timestamp_ns)
}

/// Mirror of [`nearest_preceding`]: the earliest matching candidate at or
/// after `anchor_ts`, but no more than `MATCH_WINDOW_NS` later.
fn nearest_following<'a>(
    candidates: &[&'a TraceEvent],
    anchor_ts: u64,
    pred: impl Fn(&TraceEvent) -> bool,
) -> Option<&'a TraceEvent> {
    candidates
        .iter()
        .copied()
        .filter(|e| pred(e) && e.timestamp_ns >= anchor_ts && e.timestamp_ns - anchor_ts <= MATCH_WINDOW_NS)
        .min_by_key(|e| e.timestamp_ns)
}

enum Cause<'a> {
    Futex { wait: &'a TraceEvent },
    BlockIo { io: &'a TraceEvent },
    Unknown,
}

/// What was `tid` doing that a `sched_switch`-sourced *resume* at `resume_ts`
/// was the tail end of? This is a same-incident join across two independent
/// tracepoint sources describing the same real event, and the two directions
/// are *not* symmetric:
///
/// - A block I/O completion (fired in interrupt/softirq context) is what
///   *causes* the resume -- it necessarily lands at or before `resume_ts`.
/// - A futex_wait's own `sys_exit` stamp is taken once the syscall actually
///   returns to userspace, which happens some (small) amount of *after* the
///   scheduler resumes the thread (`sched_switch`) -- so it necessarily lands
///   at or after `resume_ts`, never before. Matching this with
///   `nearest_preceding` (as an earlier version of this file did) can never
///   succeed, silently misreporting every futex-caused stall as "unknown".
fn classify_resume_cause<'a>(tid: u32, resume_ts: u64, typed: &Typed<'a>) -> Cause<'a> {
    let fw = nearest_following(&typed.futex_wait, resume_ts, |e| e.tid == tid);
    let io = nearest_preceding(&typed.block_io, resume_ts, |e| e.tid == tid);
    match (fw, io) {
        (Some(f), Some(i)) => {
            let f_dist = f.timestamp_ns.saturating_sub(resume_ts);
            let io_dist = resume_ts.saturating_sub(i.timestamp_ns);
            if f_dist <= io_dist {
                Cause::Futex { wait: f }
            } else {
                Cause::BlockIo { io: i }
            }
        }
        (Some(f), None) => Cause::Futex { wait: f },
        (None, Some(i)) => Cause::BlockIo { io: i },
        (None, None) => Cause::Unknown,
    }
}

/// What did `tid` do, immediately prior to (and by implication, in order to be
/// able to reach) `anchor_ts`? Unlike [`classify_resume_cause`], this is a
/// genuine "what already finished before this point" search -- both a futex
/// wait and a block I/O completion must have already happened, so both use
/// `nearest_preceding`. Used to keep walking the chain backward from a waker's
/// own wake call to whatever *it* was blocked on.
fn classify_prior_cause<'a>(tid: u32, anchor_ts: u64, typed: &Typed<'a>) -> Cause<'a> {
    let fw = nearest_preceding(&typed.futex_wait, anchor_ts, |e| e.tid == tid);
    let io = nearest_preceding(&typed.block_io, anchor_ts, |e| e.tid == tid);
    match (fw, io) {
        (Some(f), Some(i)) => {
            if f.timestamp_ns >= i.timestamp_ns {
                Cause::Futex { wait: f }
            } else {
                Cause::BlockIo { io: i }
            }
        }
        (Some(f), None) => Cause::Futex { wait: f },
        (None, Some(i)) => Cause::BlockIo { io: i },
        (None, None) => Cause::Unknown,
    }
}

pub struct DevNames {
    cache: HashMap<u32, String>,
}

impl DevNames {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Resolve a raw kernel `dev_t` (as carried in the block tracepoints,
    /// `major = dev >> 20, minor = dev & 0xFFFFF`) to a device name like "sda1"
    /// via the matching `/sys/dev/block/<major>:<minor>` symlink. Falls back to
    /// "devMAJ:MIN" if sysfs doesn't have an entry (e.g. the device was
    /// removed since the trace was captured).
    pub fn name(&mut self, dev: u32) -> String {
        if let Some(n) = self.cache.get(&dev) {
            return n.clone();
        }
        let major = dev >> 20;
        let minor = dev & 0xFFFFF;
        let name = std::fs::read_link(format!("/sys/dev/block/{major}:{minor}"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .unwrap_or_else(|| format!("dev{major}:{minor}"));
        self.cache.insert(dev, name.clone());
        name
    }
}

fn format_wall_time(unix_ns: u64) -> String {
    let secs = (unix_ns / 1_000_000_000) as libc::time_t;
    let millis = (unix_ns % 1_000_000_000) / 1_000_000;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&secs, &mut tm);
    }
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        tm.tm_hour, tm.tm_min, tm.tm_sec, millis
    )
}

fn format_duration(ns: u64) -> String {
    if ns >= 1_000_000_000 {
        format!("{:.2}s", ns as f64 / 1_000_000_000.0)
    } else if ns >= 1_000_000 {
        format!("{}ms", ns / 1_000_000)
    } else if ns >= 1_000 {
        format!("{}\u{b5}s", ns / 1_000)
    } else {
        format!("{ns}ns")
    }
}

/// One fully-formatted causal chain, headline first.
pub struct Chain {
    pub lines: Vec<String>,
}

fn explain_one_stall(block: &TraceEvent, trace: &TraceFile, typed: &Typed, dev_names: &mut DevNames) -> Chain {
    let start_ts = block.timestamp_ns.saturating_sub(block.duration_ns);
    let wall = format_wall_time(trace.wall_ns_for(start_ts));

    let headline_cause = classify_resume_cause(block.tid, block.timestamp_ns, typed);
    let mut lines = Vec::new();

    match &headline_cause {
        Cause::Futex { wait } => {
            lines.push(format!(
                "{wall} \u{2014} tid {} blocked {} on futex {:#x}",
                block.tid,
                format_duration(block.duration_ns),
                wait.key
            ));
            walk_from_futex_wait(wait, typed, dev_names, 1, &mut lines);
        }
        Cause::BlockIo { io } => {
            let (dev, sector) = unpack_block_key(io.key);
            lines.push(format!(
                "{wall} \u{2014} tid {} blocked {} on block I/O (dev {}, sector {sector})",
                block.tid,
                format_duration(block.duration_ns),
                dev_names.name(dev)
            ));
        }
        Cause::Unknown => {
            lines.push(format!(
                "{wall} \u{2014} tid {} blocked {} (no correlated cause found)",
                block.tid,
                format_duration(block.duration_ns)
            ));
        }
    }

    Chain { lines }
}

fn walk_from_futex_wait(
    wait: &TraceEvent,
    typed: &Typed,
    dev_names: &mut DevNames,
    depth: u32,
    lines: &mut Vec<String>,
) {
    let Some(wake) = nearest_preceding(&typed.futex_wake, wait.timestamp_ns, |e| e.key == wait.key) else {
        return; // no wake in-window found; chain ends, unexplained.
    };
    lines.push(format!(" \u{2190} woken by tid {}", wake.tid));

    if depth >= MAX_CHAIN_DEPTH {
        return;
    }

    // The waker's own wake() syscall is brief; anchor the search for what the
    // waker itself was doing right before waking us at the *start* of that
    // syscall, not its end.
    let wake_start_ts = wake.timestamp_ns.saturating_sub(wake.duration_ns);
    match classify_prior_cause(wake.tid, wake_start_ts, typed) {
        Cause::Futex { wait: next_wait } => {
            lines.push(format!(
                " \u{2190} tid {} blocked {} on futex {:#x}",
                wake.tid,
                format_duration(next_wait.duration_ns),
                next_wait.key
            ));
            walk_from_futex_wait(next_wait, typed, dev_names, depth + 1, lines);
        }
        Cause::BlockIo { io } => {
            let (dev, sector) = unpack_block_key(io.key);
            lines.push(format!(
                " \u{2190} tid {} blocked {} on block I/O (dev {}, sector {sector})",
                wake.tid,
                format_duration(io.duration_ns),
                dev_names.name(dev)
            ));
        }
        Cause::Unknown => {
            // The waker exists and woke us, but we can't further explain what
            // it itself was doing (e.g. it wasn't blocked, or the tracked
            // window didn't capture it). End the chain here.
        }
    }
}

/// Find the `slowest` longest blocked intervals and explain each one's
/// causal chain.
pub fn explain(trace: &TraceFile, slowest: usize) -> Vec<Chain> {
    let typed = Typed::partition(&trace.events);
    let mut blocks = typed.block.clone();
    blocks.sort_by(|a, b| b.duration_ns.cmp(&a.duration_ns));
    blocks.truncate(slowest);

    let mut dev_names = DevNames::new();
    blocks
        .into_iter()
        .map(|b| explain_one_stall(b, trace, &typed, &mut dev_names))
        .collect()
}

pub fn print_chains(chains: &[Chain]) {
    if chains.is_empty() {
        println!("whyslow: no blocked intervals recorded (process may not have stalled, or trace is empty)");
        return;
    }
    for (i, chain) in chains.iter().enumerate() {
        if i > 0 {
            println!();
        }
        for line in &chain.lines {
            println!("{line}");
        }
    }
}
