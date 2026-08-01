# Design

The README stays short on purpose. This is where the actual reasoning lives.

## Layout

```
whyslow-common/   Event schema shared by both sides below (no_std, no deps).
whyslow-ebpf/     eBPF programs (aya), compiled to bpfel-unknown-none,
                  embedded into the CLI binary at build time.
whyslow-cli/      Userspace: loads/attaches BPF, drains the ring buffer,
                  runs causal inference, formats `explain` output.
synthetic/        Ground-truth reproducer used by the integration test.
```

`whyslow-common` exists purely to hold the `#[repr(C)]` event struct both the
`no_std` eBPF side and the `std` userspace side need identical layout for —
standard shape for an `aya` project (see
[aya-template](https://github.com/aya-rs/aya-template)).

## Event sources

Three tracepoint families, one eBPF program each, in `whyslow-ebpf/src/main.rs`:

| Tracepoint | Program | What it produces |
|---|---|---|
| `sched:sched_switch` | `sched_switch` | thread went off-CPU → resumed running |
| `syscalls:sys_enter_futex` / `sys_exit_futex` | `futex_enter` / `futex_exit` | a `FUTEX_WAIT`/`FUTEX_WAKE` syscall's full span |
| `block:block_rq_issue` / `block_rq_complete` | `block_rq_issue` / `block_rq_complete` | a block I/O request's issue→complete latency |

All five write into one shared `RingBuf` map, as fixed-size 56-byte
`whyslow_common::Event` records. No heap allocation on the BPF side —
in-flight state ("tid 4821 entered futex_wait on addr X") lives in small
hash maps keyed by tid or `(dev, sector)`, looked up rather than allocated
when the matching event arrives.

Every `Event` is stamped at the moment its span **completed**, never at the
start. That matters for the matching logic below.

## Kernel struct layout: mirrored, not CO-RE

`whyslow-ebpf/src/vmlinux.rs` hand-mirrors the four `trace_event_raw_*`
structs it reads, transcribed field-for-field from this machine's kernel BTF
(`bpftool btf dump file /sys/kernel/btf/vmlinux format c`). Rust's
`#[repr(C)]` computes the same offsets a C compiler would given the same
field list, so nothing is hand-computed.

Not using CO-RE relocation here, on purpose:

1. These structs back the ftrace/tracepoint ABI, which is already a stable
   userspace contract — the same layout `perf`/`bpftrace`/`bcc` have parsed
   for years without CO-RE. It won't move under us.
2. `aya-tool generate` needs `bindgen` → `libclang` at build time, an extra
   toolchain cost not worth it for v1. Swapping in generated bindings later
   is a one-file change if it's ever needed.

## How the causal chain gets built

This is the part correctness actually depends on. Every edge is a
bounded-window nearest-match join, one of two directions:

- `nearest_preceding(candidates, anchor, window=5ms)`: latest candidate at or
  before `anchor`, no more than 5ms earlier.
- `nearest_following(candidates, anchor, window=5ms)`: earliest candidate at
  or after `anchor`, no more than 5ms later.

Nothing in range → no edge, chain ends there. The 5ms bound exists so a
stale event from an earlier, unrelated contention cycle can't get matched
to the wrong wait.

**1. What was a blocked interval actually blocked on?** `sched_switch` tells
you a thread went off-CPU, not why. This joins two different tracepoint
sources describing the same moment, and the direction isn't symmetric:

- A block I/O completion (fires in interrupt context) *causes* the resume —
  it lands at or before it, so: `nearest_preceding`.
- A `futex_wait`'s exit timestamp is stamped once the syscall returns to
  userspace, which is a few instructions *after* the scheduler already
  resumed the thread — so: `nearest_following`. Using `nearest_preceding`
  here (an earlier version did) can never match, since the candidate is
  always after the anchor — every futex-caused stall silently came back
  "no correlated cause found." The synthetic test caught this.

If both match, whichever's closer wins.

**2. What woke a futex wait?** The `futex_wake` on the same address whose
own completion is the nearest one preceding the wait's completion, within
5ms. This direction *is* the intuitive one — the wake call marks the target
runnable synchronously, so it completes at or before the wait does.

**3. What was the waker doing right before it woke us?** Recurse: same
question as step 1, anchored at the *start* of the waker's `futex_wake`
call. This is a plain "what already finished before this point" lookup, not
a same-moment join, so both candidates use `nearest_preceding` here.
Continues until it hits a `block_io` event (always a leaf — nothing further
upstream is instrumented), finds nothing, or hits a depth guard of 8 hops.

**Two calls made explicitly rather than guessed:**
- Wake↔wait matching is nearest-preceding within a bounded window, not
  unbounded and not requiring a third corroborating event. Simpler, and the
  unbounded version risks matching a stale wake.
- Block I/O root cause assumes synchronous I/O: the tid that issued
  `block_rq_issue` is the one that's off-CPU until `block_rq_complete`.
  `block_rq_complete` fires in interrupt context, so the issuer's identity
  is captured at issue time and carried forward, never re-derived from
  "current" at completion.

## Known limitations (v1, on purpose)

- **Block key collisions**: `(dev, sector)` packs into 64 bits with sector
  truncated to 32 bits. Fine up to 2TiB devices at 512B sectors, more at 4Kn.
- **Request splitting**: correlates block I/O by `(dev, sector)`, not request
  pointer (the tracepoints don't expose one). A split/merged request can
  produce multiple `block_io` events instead of one; the walk picks the
  closest match, usually right but not guaranteed under adversarial merge
  patterns. The synthetic test avoids this with a single small `O_DIRECT`
  read that stays under typical `max_sectors_kb`.
- **Run-queue delay isn't tracked.** Only genuinely-blocked (non-`TASK_RUNNING`)
  time counts as a stall — a thread preempted mid-timeslice but still
  runnable doesn't show up. Real cause of slowness, different one, left for
  a future event source.
- **`attach` races the target**: `run` forks the child stopped (via
  `PTRACE_TRACEME`, so it traps right after `execve`) and attaches BPF
  before letting it run — nothing is missed. `attach <pid>` targets a
  process already running, so there's an unavoidable small window before
  BPF is live.
- **`attach` wants a tgid**, not an arbitrary thread id — passing a
  non-leader tid of a multithreaded process traces nothing.

## Ground truth

`synthetic/` is a two-thread reproducer with a known chain: a "holder"
thread takes a raw-futex lock, then does a cold `O_DIRECT` disk read (real
block I/O, not page-cache-served); a "waiter" thread contends for the same
lock and genuinely blocks until `unlock()` wakes it.
`whyslow-cli/tests/integration.rs` runs it under `whyslow run` and checks
the printed chain names the waiter as futex-blocked, the holder as the
waker, and the holder's own stall as block I/O.
