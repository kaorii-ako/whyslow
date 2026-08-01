//! `whyslow run` needs BPF attached (and the target tgid filter set) before
//! the child executes a single instruction, or its early activity is invisible
//! to the tracer.
//!
//! First attempt was having the child raise `SIGSTOP` on itself in `pre_exec`,
//! before calling `execve()`. That deadlocks: `std::process::Command::spawn()`
//! itself blocks on the parent side waiting for the child to reach `execve()`
//! (or fail trying), signaled via an internal `O_CLOEXEC` pipe -- but a child
//! that stops itself pre-exec never gets there, so `spawn()` never returns.
//!
//! Instead we use `PTRACE_TRACEME` in `pre_exec`: the kernel then delivers an
//! automatic `SIGTRAP` stop to the child immediately *after* `execve()`
//! completes but before the new program's first instruction runs. `execve()`
//! itself still completes normally, so `spawn()`'s pipe closes and it returns
//! as usual; we then see the trap-stop via a normal `waitpid()`. `attach <pid>`
//! has no equivalent: it inherently races with whatever the target is already
//! doing (documented in README.md).

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

use anyhow::{Context, anyhow};

pub fn spawn_stopped(argv: &[String]) -> anyhow::Result<Child> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    unsafe {
        cmd.pre_exec(|| {
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
        .with_context(|| format!("spawning `{}`", argv.join(" ")))
}

/// Block until `pid` hits its post-exec `PTRACE_TRACEME` trap (see module
/// docs), or return an error if it exited first.
pub fn wait_until_stopped(pid: u32) -> anyhow::Result<()> {
    loop {
        let mut status: libc::c_int = 0;
        let ret = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WUNTRACED) };
        if ret < 0 {
            return Err(anyhow!("waitpid({pid}) failed: {}", std::io::Error::last_os_error()));
        }
        if libc::WIFSTOPPED(status) {
            return Ok(());
        }
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            return Err(anyhow!("child {pid} exited before tracing could start"));
        }
    }
}

/// Detach ptrace and let the child run free from right after its exec.
pub fn resume(pid: u32) -> anyhow::Result<()> {
    if unsafe { libc::ptrace(libc::PTRACE_DETACH, pid as libc::pid_t, 0, 0) } != 0 {
        return Err(anyhow!(
            "PTRACE_DETACH({pid}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Non-blocking "has this pid exited yet?", reaping it if so. Suitable to
/// call repeatedly from a `should_stop` poll loop.
pub fn has_exited(pid: u32) -> bool {
    let mut status: libc::c_int = 0;
    let ret = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    ret == pid as libc::pid_t
}
