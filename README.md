# whyslow

Debug *why* a Linux process was slow by correlating multiple kernel event
sources — context switches, futex contention, block I/O — into one causal
chain, instead of the disjoint traces `strace`/`perf`/`py-spy` give you in
isolation.

```
$ sudo whyslow run -- ./my-slow-program

14:32:07.001 — tid 4821 blocked 412ms on futex 0x7f2a3c001000
 ← woken by tid 4809
 ← tid 4809 blocked 380ms on block I/O (dev nvme0n1p2, sector 88213)
```

**whyslow requires root or `CAP_BPF` to run**, on any install method. It
loads eBPF programs, which the kernel restricts to privileged processes.
Symptoms of not having it: `whyslow run` fails immediately with a permission
error (we check `geteuid()` up front and say so explicitly, rather than
failing silently). This will *not* work out of the box in: most containers
(need `--cap-add=BPF --cap-add=PERFMON` or `--privileged`), some restricted
cloud shells, and WSL1 (WSL2 with a kernel built with `CONFIG_BPF`/BTF works).

Phase 1 (this codebase) targets **x86_64 Linux, kernel 5.8+** (ring buffer
map support). No stack symbolication yet — threads/processes are identified
by tid/pid + comm name only.

## Installing

Prebuilt binaries (x86_64, aarch64):

```
curl -sSf https://raw.githubusercontent.com/kaorii-ako/whyslow/main/install.sh | sh
```

Or, if it's already installed to a place your shell can't find it:

```
curl -sSf https://raw.githubusercontent.com/kaorii-ako/whyslow/main/install.sh | WHYSLOW_INSTALL_DIR=/usr/local/bin sh
```

**Homebrew** (Linux only):
```
brew install kaorii-ako/whyslow/whyslow
```

**APT** (unsigned repo — see [DISTRIBUTION.md](DISTRIBUTION.md) for why, and
the plan to sign it):
```
echo "deb [trusted=yes] https://kaorii-ako.github.io/whyslow/apt ./" \
  | sudo tee /etc/apt/sources.list.d/whyslow.list
sudo apt update && sudo apt install whyslow
```

**Cargo** — `cargo install whyslow` does *not* work (registry installs can't
see `whyslow-ebpf` as a workspace sibling, which the build needs — see
[DISTRIBUTION.md](DISTRIBUTION.md)). Install from git instead, which does
work, and still needs a nightly toolchain + `bpf-linker` on *your* machine
too (see "Building" below — a real cost of building an eBPF tool, not a bug):
```
cargo install --git https://github.com/kaorii-ako/whyslow whyslow
```

**Arch (AUR)**: `whyslow-bin` — see `packaging/aur/PKGBUILD`.

**pip**: `pip install whyslow` — thin wrapper that fetches the same prebuilt
binary; see `packaging/pypi/`.

Whichever way you install it, **whyslow still needs root or CAP_BPF to run**
(see above) — the installer/package managers just get the binary onto your
machine, they can't grant that at install time.

## Architecture

```
whyslow-common/   Event schema shared by both sides below (no_std, no deps).
whyslow-ebpf/     eBPF programs (aya), compiled to bpfel-unknown-none,
                  embedded into the CLI binary at build time.
whyslow-cli/      Userspace: loads/attaches BPF, drains the ring buffer,
                  runs causal inference, formats `explain` output.
synthetic/        Ground-truth reproducer used by the integration test.
```

### Workspace layout note

The task brief asked for two crates (`whyslow-ebpf`, `whyslow-cli`). We added
a third, `whyslow-common`, purely to hold the `#[repr(C)]` event struct that
both the `no_std` eBPF side and the `std` userspace side need identical
layout for — this is the standard aya project shape (see
[aya-template](https://github.com/aya-rs/aya-template)), not scope creep.

### Event sources

Three tracepoint families, exactly as specified, each with a single-purpose
eBPF program in `whyslow-ebpf/src/main.rs`:

| Tracepoint | Program | What it produces |
|---|---|---|
| `sched:sched_switch` | `sched_switch` | thread went off-CPU (blocked) → resumed running |
| `syscalls:sys_enter_futex` / `sys_exit_futex` | `futex_enter` / `futex_exit` | a `FUTEX_WAIT`/`FUTEX_WAKE` syscall's full span |
| `block:block_rq_issue` / `block_rq_complete` | `block_rq_issue` / `block_rq_complete` | a block I/O request's issue→complete latency |

All five programs write into **one shared `RingBuf` map** (`EVENTS`), as
fixed-size `whyslow_common::Event` records (56 bytes, `#[repr(C)]`,
`Copy`). No heap allocation on the BPF side anywhere — per-thread/per-request
"in-flight" state (e.g. "tid 4821 entered `futex_wait` at ts=X on addr=Y")
lives in small `BPF_MAP_TYPE_HASH` maps keyed by tid or by a packed
`(dev, sector)`, and is looked up, not allocated, when the matching
enter/exit or issue/complete arrives.

Every emitted `Event` is stamped with the timestamp at which its span
**completed** — thread resumed running, syscall returned, I/O completed —
never at the start. This matters for the matching logic below.

### Kernel struct layout: mirrored, not CO-RE

`whyslow-ebpf/src/vmlinux.rs` hand-mirrors the four `trace_event_raw_*`
structs we read (`sched_switch`, the generic `sys_enter`/`sys_exit`
templates, and the two block-request templates), transcribed field-for-field
from this machine's own kernel BTF
(`bpftool btf dump file /sys/kernel/btf/vmlinux format c`). Rust's
`#[repr(C)]` computes the same offsets/padding a C compiler would for an
identical field list, so no byte offsets are hand-computed anywhere.

This is deliberately **not** using BTF CO-RE relocation (`aya-tool
generate` + `Object::relocate_btf`). Two reasons:

1. The `trace_event_raw_*` structs back the ftrace/tracepoint ABI, which is
   *already* a stable, versioned userspace contract — it's the same
   `/sys/kernel/tracing/events/.../format` layout every `perf`, `bpftrace`,
   and `bcc` tool has parsed for years without CO-RE. Field reordering here
   would break every one of those tools too.
2. `aya-tool generate` shells out to `bindgen`, which needs `libclang` at
   dev-machine build time — an extra toolchain dependency we didn't want to
   impose for a first pass. If a future contributor wants full CO-RE
   portability (e.g. to also support architectures with different
   `long`/pointer widths in these structs, or kernels with debug configs
   that add fields), swapping in generated bindings is a localized change:
   replace the contents of `vmlinux.rs`, everything else is unaffected.

### How happens-before edges are inferred

This is the part correctness hinges on, so it's spelled out precisely. Every
edge below is built from a bounded-window nearest-match join, in one of two
directions:

> `nearest_preceding(candidates, anchor_ts, window = 5ms)`: the candidate
> whose completion timestamp is the latest one at or before `anchor_ts`, but
> no more than 5ms earlier.
> `nearest_following(candidates, anchor_ts, window = 5ms)`: the mirror image
> — the earliest one at or after `anchor_ts`, but no more than 5ms later.
>
> Outside the window, or with no candidate at all, there is no edge — the
> chain simply ends.

The 5ms bound exists so a stale event from an earlier, unrelated contention
cycle can never get matched to the wrong wait (unbounded matching was
considered and rejected for exactly this reason).

**1. A blocked interval → what it was blocked *on*.** `sched_switch` tells us
*that* tid T was off-CPU for some span, not *why*. This step joins **two
different tracepoint sources describing the same real-world moment**, and
the two candidate causes are *not* symmetric in which direction to search:

- A `block_io` completion fires in interrupt/softirq context and is what
  *causes* the resume — it necessarily lands at or **before** the moment T
  resumes, so we use `nearest_preceding`.
- A `futex_wait`'s own exit timestamp is stamped once the syscall actually
  returns to userspace, which happens some (small) amount of time **after**
  the scheduler has already resumed T (`sched_switch` fires first, purely as
  a scheduling decision; the syscall-return instrumentation point comes a few
  instructions later) — so we use `nearest_following`. Using
  `nearest_preceding` here (an earlier version of this file did) can never
  match, since the candidate is always chronologically after the anchor —
  silently misreporting every futex-caused stall as "no correlated cause
  found". This was caught by the synthetic ground-truth test, precisely the
  kind of bug that test exists to catch.

If both a `futex_wait` and a `block_io` match, whichever ends closer to the
resume time wins; if neither, the stall is reported with an unknown cause.

**2. A futex wait → the wake that ended it.** Once we know T was in
`futex_wait` on address A, we look for the `futex_wake` on address A whose
own completion is the `nearest_preceding` one to T's wait completion, within
the 5ms window. Here the direction *is* the intuitive one: the wake syscall
marks the target runnable synchronously within the call, so it completes at
or before the corresponding wait can complete. This is the literal
"`futex_wait` on tid A ending exactly when a `futex_wake` from tid B is
emitted implies a causal edge B→A" rule from the spec, made precise: kernel
wakeup scheduling latency means "exactly" never holds at the nanosecond
level, so we match the nearest preceding completion within a bound instead
of requiring equality.

**3. A waker's own wake → what *it* was blocked on.** The thread that issued
the wake (tid B) had to resume from *something* before it could call
`futex_wake` — so we recurse, anchored at the *start* of B's wake syscall
(its own syscall duration is usually negligible, but we anchor at the start,
not the end, since that's closest to the actual moment B became active
again). Unlike step 1, this is a plain "what already finished before this
point" search, not a same-incident join across two sources — so both
candidate causes use `nearest_preceding` here. This walk continues until it
terminates at a `block_io` event (block I/O is always a leaf/root cause in
v1 — the spec's own example stops there, and BPF only instruments the two
block tracepoints, not e.g. IRQ delivery, so there's nothing further
upstream to attribute it to), until no further cause is found, or after 8
hops (cycle guard; not expected to ever bind in practice).

**Design decisions confirmed with the requester before implementation**
(both were flagged as the highest-risk logic, per project instructions to
ask rather than guess):

- *Wake↔wait matching*: nearest-preceding wake within a bounded window (5ms),
  not unbounded, and not requiring corroboration via a third event. Chosen
  for simplicity; the unbounded variant risks misattributing to a stale wake,
  the corroborated variant needs a third event source for a v1 that's meant
  to keep scope to three sources.
- *Block I/O root attribution*: assumes synchronous I/O — the tid that issued
  `block_rq_issue` is the same tid that's off-CPU until the matching
  `block_rq_complete`. `block_rq_complete` fires in interrupt/softirq
  context, so the issuing thread's identity is captured at issue time and
  carried in the `BLOCKIO_START` map, never re-derived from "current" at
  completion time.

### Known v1 limitations (by design, not oversight)

- **Block key collisions**: `Event.key` packs `(dev, sector)` into 64 bits
  as `dev << 32 | (sector & 0xFFFFFFFF)`, truncating `sector_t` to its low 32
  bits. Fine up to 2^32 sectors (2TiB at 512B sectors, more at 4Kn); a
  pathologically large device could theoretically alias two in-flight
  requests. Not observed in practice on any device size in the field today.
- **Request splitting**: `block_rq_issue`/`block_rq_complete` correlate by
  `(dev, sector)`, not by request pointer (which those two tracepoints don't
  expose). A single logical I/O that the block layer splits or that
  completes in multiple partial chunks will show up as multiple `block_io`
  events rather than one; the causal walk picks the one ending closest to
  the anchor, which is usually the right one to blame but isn't guaranteed
  in adversarial merge/split patterns. The synthetic test sidesteps this by
  using a single small `O_DIRECT` read that stays under typical
  `max_sectors_kb`, so it reliably produces exactly one issue/complete pair.
- **Off-CPU while runnable (run-queue delay) isn't tracked.** Only
  non-`TASK_RUNNING` off-CPU time counts as a "blocked interval"; a thread
  preempted mid-timeslice but still runnable doesn't generate a stall. This
  keeps v1 scoped to the futex/block-I/O causes the spec's example is about;
  scheduler run-queue latency is a real "why slow" cause too, but a
  different one, left for a future event source rather than conflated here.
- **`attach <pid>` has a startup race** that `run` does not: `run` forks the
  child, has it `SIGSTOP` itself immediately post-fork (before `execve`),
  and only then loads/attaches BPF and sets the target tgid before
  `SIGCONT`ing it — so tracing is complete from the child's very first
  instruction. `attach` targets an already-running process, so there's an
  inherent (small, unavoidable for any attach-style tracer) window between
  "process is doing things" and "BPF is attached."
- **`attach <pid>` expects a tgid** (i.e. what `/proc/<pid>` calls a
  process), not an arbitrary thread id of a multithreaded process. Passing a
  non-leader tid will trace nothing (the tgid filter in-kernel won't match).

## Usage

```
whyslow run [--trace-out PATH] [--slowest N] -- <command> [args...]
whyslow attach <pid> [--trace-out PATH] [--slowest N] [--duration-secs S]
whyslow explain [--slowest N] [--trace PATH]
```

`run`/`attach` both trace, write the raw event trace to `--trace-out`
(default `./whyslow.trace.json`), and immediately print the top `--slowest`
causal chains (default 3). `explain` re-runs just the causal-inference +
formatting step against a previously captured trace file, for offline
reanalysis without re-running the traced program.

`attach` with no `--duration-secs` traces until Ctrl-C.

## Building

Requires, in addition to stable Rust for the CLI:

- A **nightly** toolchain with the `rust-src` component, for the
  `whyslow-ebpf` crate specifically (pinned via its own
  `rust-toolchain.toml`, doesn't affect the rest of the workspace).
- [`bpf-linker`](https://github.com/aya-rs/bpf-linker) (`cargo install
  bpf-linker`), which itself needs system LLVM development libraries
  (`clang`/`llvm-*-dev`) installed via your package manager.

```
cargo build            # builds whyslow-ebpf via build.rs, then whyslow-cli
sudo -E cargo test     # integration test needs root/CAP_BPF to load BPF
sudo ./target/debug/whyslow run -- echo hi
```

The workspace's `.cargo/config.toml` sets the test/run `runner` to
`sudo -E`, so plain `cargo run`/`cargo test` already execute as root; you'll
be prompted for your password the first time in a given terminal/session.

## Validating against ground truth

`synthetic/` is a two-thread reproducer with a known causal chain: a
"holder" thread takes a raw-futex lock, then does a cold `O_DIRECT` disk
read (guaranteed real block I/O, not page-cache-served); a "waiter" thread
contends for the same lock and genuinely blocks in `futex(FUTEX_WAIT)` until
holder's `unlock()` wakes it. `whyslow-cli/tests/integration.rs` runs it
under `whyslow run` and asserts the printed chain names the waiter as
blocked on a futex, names the holder as the waker, and names the holder's
own stall as block I/O.
