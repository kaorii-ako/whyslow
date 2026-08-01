# whyslow

Finds out *why* a Linux process was slow — not just that it was. The actual
chain: this thread was stuck here, because that thread woke it late, because
that thread was stuck on a disk read.

```
$ sudo whyslow run -- ./my-slow-program

14:32:07.001 — tid 4821 blocked 412ms on futex 0x7f2a3c001000
 ← woken by tid 4809
 ← tid 4809 blocked 380ms on block I/O (dev nvme0n1p2, sector 88213)
```

`strace`, `perf`, and `py-spy` each show you one piece of this. whyslow
watches `sched_switch`, futex, and block I/O events via eBPF and stitches
them into one chain, automatically.

## Install

```
curl -sSf https://raw.githubusercontent.com/kaorii-ako/whyslow/main/install.sh | sh
```

Also on Homebrew, APT, and pip — see [DISTRIBUTION.md](DISTRIBUTION.md).

**Needs root or `CAP_BPF`.** It's loading eBPF programs; the kernel doesn't
let unprivileged processes do that. Won't work out of the box in most
containers (add `--cap-add=BPF --cap-add=PERFMON`), some cloud shells, or
WSL1.

## Use

```
sudo whyslow run -- <command>                  # trace a new process
sudo whyslow attach <pid>                      # trace one already running
whyslow explain --trace whyslow.trace.json     # re-read a saved trace
```

## Build it

```
rustup toolchain install nightly --component rust-src
cargo install bpf-linker   # also needs clang/llvm-dev on your system
cargo build
sudo ./target/debug/whyslow run -- echo hi
```

`cargo install whyslow` doesn't work (registry installs lose the workspace
the build needs) — `cargo install --git https://github.com/kaorii-ako/whyslow whyslow`
does.

## How it works, and what it doesn't handle yet

Three eBPF programs, one ring buffer, timestamps matched backward until the
trail runs out. Full writeup, matching rules, and known gaps: [DESIGN.md](DESIGN.md).

x86_64 + aarch64 Linux, kernel 5.8+. No stack symbolication yet — just
tid/pid/comm.

## AI Usage

Mosty for debugging and some for code gen.

## License

MIT or Apache-2.0, pick one.
