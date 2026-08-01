use which::which;

/// Building this crate has an undeclared dependency on the `bpf-linker` binary. This
/// would be better expressed by artifact-dependencies but that's not practical yet
/// (see rust-lang/cargo#12385). This causes cargo to rebuild whenever the mtime of
/// `which bpf-linker` changes, which is an imperfect but workable cache-invalidation
/// signal.
fn main() {
    let bpf_linker = which("bpf-linker").unwrap();
    println!("cargo:rerun-if-changed={}", bpf_linker.to_str().unwrap());
}
