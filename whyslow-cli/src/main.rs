mod bpf;
mod causal;
mod child;
mod trace;
mod ui;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, anyhow, bail};
use clap::{Parser, Subcommand};

use trace::TraceFile;

#[derive(Parser)]
#[command(name = "whyslow", about = "Debug why a Linux process was slow")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Launch and trace a child process.
    Run {
        /// Path to write the raw trace to, for later `whyslow explain --trace`.
        #[arg(long, default_value = "whyslow.trace.json")]
        trace_out: PathBuf,
        /// How many causal chains to print once the child exits.
        #[arg(long, default_value_t = 3)]
        slowest: usize,
        /// Command to run, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Attach to an already-running process.
    Attach {
        /// Process id (thread group id) to trace.
        pid: u32,
        /// Path to write the raw trace to, for later `whyslow explain --trace`.
        #[arg(long, default_value = "whyslow.trace.json")]
        trace_out: PathBuf,
        /// How many causal chains to print when tracing stops.
        #[arg(long, default_value_t = 3)]
        slowest: usize,
        /// Trace for this many seconds, then stop automatically. Default:
        /// trace until Ctrl-C.
        #[arg(long)]
        duration_secs: Option<u64>,
    },
    /// Print the top N causal stalls from a previously captured trace.
    Explain {
        #[arg(long, default_value_t = 3)]
        slowest: usize,
        #[arg(long, default_value = "whyslow.trace.json")]
        trace: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            trace_out,
            slowest,
            command,
        } => run(command, &trace_out, slowest),
        Command::Attach {
            pid,
            trace_out,
            slowest,
            duration_secs,
        } => attach(pid, &trace_out, slowest, duration_secs),
        Command::Explain { slowest, trace } => explain_cmd(&trace, slowest),
    }
}

fn require_root() -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!(
            "whyslow needs root or CAP_BPF to load its eBPF programs. \
             Try again with sudo (this includes containers / restricted cloud \
             shells / WSL without CAP_BPF configured -- see README.md)."
        );
    }
    Ok(())
}

fn run(command: Vec<String>, trace_out: &std::path::Path, slowest: usize) -> anyhow::Result<()> {
    require_root()?;
    ui::print_banner();
    println!("{} {}", ui::blue("\u{25b8} tracing:"), ui::bold(&command.join(" ")));

    let child = child::spawn_stopped(&command)?;
    let pid = child.id();
    child::wait_until_stopped(pid).context("waiting for child to reach pre-exec stop point")?;

    let mut ebpf = bpf::load_and_attach()?;
    bpf::set_target_tgid(&mut ebpf, pid)?;

    let (realtime_anchor_ns, monotonic_anchor_ns) = TraceFile::capture_anchor();
    child::resume(pid).context("resuming child after attaching BPF")?;

    let events = bpf::drain_ring_buffer(&mut ebpf, || child::has_exited(pid))?;

    let trace_file = TraceFile {
        realtime_anchor_ns,
        monotonic_anchor_ns,
        events,
    };
    trace_file
        .write_to(trace_out)
        .with_context(|| format!("writing trace to {}", trace_out.display()))?;

    println!();
    causal::print_chains(&causal::explain(&trace_file, slowest));
    Ok(())
}

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

fn attach(
    pid: u32,
    trace_out: &std::path::Path,
    slowest: usize,
    duration_secs: Option<u64>,
) -> anyhow::Result<()> {
    require_root()?;
    ui::print_banner();

    if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return Err(anyhow!("no such process: {pid}"));
    }

    let mut ebpf = bpf::load_and_attach()?;
    bpf::set_target_tgid(&mut ebpf, pid)?;
    let (realtime_anchor_ns, monotonic_anchor_ns) = TraceFile::capture_anchor();

    unsafe { libc::signal(libc::SIGINT, handle_sigint as *const () as libc::sighandler_t) };
    println!("whyslow: tracing pid {pid}, press Ctrl-C to stop and print results...");

    let deadline = duration_secs.map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    let events = bpf::drain_ring_buffer(&mut ebpf, || {
        if STOP_REQUESTED.load(Ordering::SeqCst) {
            return true;
        }
        if let Some(deadline) = deadline {
            if std::time::Instant::now() >= deadline {
                return true;
            }
        }
        // If the target process itself exits, keep draining (its comm/pid may
        // still be referenced by in-flight events) but stop once nothing is
        // left to trace: unsafe { libc::kill(pid, 0) } != 0 means gone.
        unsafe { libc::kill(pid as libc::pid_t, 0) != 0 }
    })?;

    let trace_file = TraceFile {
        realtime_anchor_ns,
        monotonic_anchor_ns,
        events,
    };
    trace_file
        .write_to(trace_out)
        .with_context(|| format!("writing trace to {}", trace_out.display()))?;

    println!();
    causal::print_chains(&causal::explain(&trace_file, slowest));
    Ok(())
}

fn explain_cmd(trace: &std::path::Path, slowest: usize) -> anyhow::Result<()> {
    let trace_file = TraceFile::read_from(trace)
        .with_context(|| format!("reading trace from {}", trace.display()))?;
    causal::print_chains(&causal::explain(&trace_file, slowest));
    Ok(())
}
