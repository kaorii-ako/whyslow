//! End-to-end ground-truth test: runs the synthetic futex+block-I/O
//! reproducer (see ../../synthetic/src/main.rs) under `whyslow run` and
//! asserts the printed causal chain matches the known cause.
//!
//! Requires root/CAP_BPF. The workspace's `.cargo/config.toml` sets
//! `runner = "sudo -E"` for all targets, so a plain `cargo test` should
//! already invoke this test binary as root; if it isn't (e.g. invoked
//! directly), the test skips itself with a clear message rather than failing.

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("whyslow-cli has a parent directory")
        .to_path_buf()
}

/// `/etc/passwd` lookup for a user's home directory, via `getpwnam(3)`.
fn passwd_home_dir(username: &str) -> Option<PathBuf> {
    let c_name = std::ffi::CString::new(username).ok()?;
    let pw = unsafe { libc::getpwnam(c_name.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr((*pw).pw_dir) };
    Some(PathBuf::from(dir.to_string_lossy().into_owned()))
}

/// This test binary itself may be running as root via our own
/// `.cargo/config.toml` `runner = "sudo -E"` (applied by cargo to *every*
/// binary/test artifact, regardless of whether the invoking shell was already
/// root). `sudo`'s `secure_path`/`always_set_home` reset `PATH`/`HOME`
/// unconditionally -- even under `-E` -- so by the time we're here, `HOME`
/// may be `/root` rather than the real invoking user's home, which breaks
/// rustup's toolchain lookup for the nested `cargo build` below. `SUDO_USER`
/// survives any number of nested sudo layers, so we use it to reconstruct a
/// correct environment rather than trusting inherited `PATH`/`HOME`.
fn sudo_corrected_env() -> Vec<(String, String)> {
    let Ok(sudo_user) = std::env::var("SUDO_USER") else {
        return Vec::new();
    };
    let Some(home) = passwd_home_dir(&sudo_user) else {
        return Vec::new();
    };
    let cargo_bin = home.join(".cargo/bin");
    let path = std::env::var("PATH").unwrap_or_default();
    vec![
        ("HOME".to_string(), home.display().to_string()),
        ("RUSTUP_HOME".to_string(), home.join(".rustup").display().to_string()),
        ("CARGO_HOME".to_string(), home.join(".cargo").display().to_string()),
        ("PATH".to_string(), format!("{}:{path}", cargo_bin.display())),
    ]
}

fn build_synthetic() -> PathBuf {
    let root = workspace_root();
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["build", "-p", "whyslow-synthetic"]).current_dir(&root);
    for (k, v) in sudo_corrected_env() {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .expect("failed to invoke cargo to build whyslow-synthetic");
    if !output.status.success() {
        eprintln!(
            "build stdout:\n{}\nbuild stderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(output.status.success(), "building whyslow-synthetic failed");
    root.join("target/debug/whyslow-synthetic")
}

#[test]
fn run_reproduces_known_causal_chain() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!(
            "SKIP: run_reproduces_known_causal_chain requires root/CAP_BPF \
             (got euid {}). Run via `sudo -E cargo test`.",
            unsafe { libc::geteuid() }
        );
        return;
    }

    let synthetic_bin = build_synthetic();
    assert!(synthetic_bin.exists(), "synthetic binary not found after build");

    let trace_out = std::env::temp_dir().join(format!(
        "whyslow-integration-test-{}.trace.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&trace_out);

    // --slowest 5, not 1: the synthetic binary's main thread also has a
    // genuine (and uninteresting) stall of its own -- pthread_join() on the
    // worker threads is itself a futex wait -- whose duration relative to
    // waiter's contended wait isn't deterministic (it spans however long the
    // whole test happens to take). Assert the known chain is *present*
    // among the top few stalls, not that it's strictly ranked #1.
    let output = Command::new(env!("CARGO_BIN_EXE_whyslow"))
        .args([
            "run",
            "--trace-out",
            trace_out.to_str().unwrap(),
            "--slowest",
            "5",
            "--",
            synthetic_bin.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run whyslow");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("--- whyslow run stdout ---\n{stdout}");
    eprintln!("--- whyslow run stderr ---\n{stderr}");

    assert!(output.status.success(), "whyslow run exited non-zero");

    let (holder_tid, waiter_tid) = parse_synthetic_tids(&stdout)
        .expect("synthetic program's SYNTHETIC_DONE line not found in output");

    assert!(
        stdout.contains(&format!("tid {waiter_tid} blocked")) && stdout.contains("on futex"),
        "expected waiter tid {waiter_tid}'s blocked-on-futex line in output:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("woken by tid {holder_tid}")),
        "expected 'woken by tid {holder_tid}' in output:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("tid {holder_tid}")) && stdout.contains("block I/O"),
        "expected holder tid {holder_tid}'s block I/O root cause in output:\n{stdout}"
    );

    let _ = std::fs::remove_file(&trace_out);
}

fn parse_synthetic_tids(output: &str) -> Option<(u32, u32)> {
    let line = output.lines().find(|l| l.starts_with("SYNTHETIC_DONE"))?;
    let mut holder = None;
    let mut waiter = None;
    for field in line.split_whitespace() {
        if let Some(v) = field.strip_prefix("holder_tid=") {
            holder = v.parse().ok();
        } else if let Some(v) = field.strip_prefix("waiter_tid=") {
            waiter = v.parse().ok();
        }
    }
    Some((holder?, waiter?))
}
