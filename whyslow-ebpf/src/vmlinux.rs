//! Hand-mirrored kernel tracepoint "raw" structs.
//!
//! These are transcribed field-for-field (same order, same primitive widths)
//! from this build machine's own kernel BTF:
//!
//! ```text
//! bpftool btf dump file /sys/kernel/btf/vmlinux format c | grep -A20 'struct trace_event_raw_...'
//! ```
//!
//! Rust's `#[repr(C)]` layout algorithm is required to compute the same
//! field offsets/padding a C compiler would for an identical field list, so we
//! don't need (or want) to hand-compute byte offsets: get the field order and
//! primitive types right and the layout follows automatically.
//!
//! Unlike arbitrary kernel-internal structs (e.g. `task_struct`), the
//! `trace_event_raw_*` structs backing tracepoints are part of the stable
//! ftrace/tracepoint ABI: their field layout is exactly what every `perf`,
//! `bpftrace`, and `bcc` tool has relied on for years via the matching
//! `/sys/kernel/tracing/events/.../format` file, and it does not change across
//! kernel versions without breaking every one of those tools. That stability
//! is why we mirror the struct directly instead of pulling in CO-RE relocation
//! machinery for v1 -- see README.md "Portability" section for the tradeoff.

#[repr(C)]
pub struct TraceEntry {
    pub kind: u16,
    pub flags: u8,
    pub preempt_count: u8,
    pub pid: i32,
}

/// `sched:sched_switch`
#[repr(C)]
pub struct TraceEventRawSchedSwitch {
    pub ent: TraceEntry,
    pub prev_comm: [u8; 16],
    pub prev_pid: i32,
    pub prev_prio: i32,
    pub prev_state: i64,
    pub next_comm: [u8; 16],
    pub next_pid: i32,
    pub next_prio: i32,
}

/// `syscalls:sys_enter_futex` (shares the generic `sys_enter` layout; `args`
/// are the six raw syscall arguments in order).
#[repr(C)]
pub struct TraceEventRawSysEnter {
    pub ent: TraceEntry,
    pub id: i64,
    pub args: [u64; 6],
}

/// `syscalls:sys_exit_futex`
#[repr(C)]
pub struct TraceEventRawSysExit {
    pub ent: TraceEntry,
    pub id: i64,
    pub ret: i64,
}

/// `block:block_rq_issue` (and `block_rq_insert`, unused here).
#[repr(C)]
pub struct TraceEventRawBlockRq {
    pub ent: TraceEntry,
    pub dev: u32,
    pub sector: u64,
    pub nr_sector: u32,
    pub bytes: u32,
    pub ioprio: u16,
    pub rwbs: [u8; 10],
    pub comm: [u8; 16],
    pub data_loc_cmd: u32,
}

/// `block:block_rq_complete`
#[repr(C)]
pub struct TraceEventRawBlockRqCompletion {
    pub ent: TraceEntry,
    pub dev: u32,
    pub sector: u64,
    pub nr_sector: u32,
    pub error: i32,
    pub ioprio: u16,
    pub rwbs: [u8; 10],
    pub data_loc_cmd: u32,
}

// futex op codes (include/uapi/linux/futex.h), stable UAPI.
pub const FUTEX_WAIT: u32 = 0;
pub const FUTEX_WAKE: u32 = 1;
pub const FUTEX_WAIT_BITSET: u32 = 9;
pub const FUTEX_WAKE_BITSET: u32 = 10;
pub const FUTEX_PRIVATE_FLAG: u32 = 128;
pub const FUTEX_CLOCK_REALTIME: u32 = 256;
pub const FUTEX_CMD_MASK: u32 = !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);

// task state (include/linux/sched.h): TASK_RUNNING is the only all-zero state;
// every other bit pattern means the task is not currently runnable. This is
// the same heuristic bcc's offcputime/runqlat tools use.
pub const TASK_RUNNING: i64 = 0;
