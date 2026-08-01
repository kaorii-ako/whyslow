# whyslow (PyPI wrapper)

This is a thin wrapper package: `pip install whyslow` gets you the `whyslow`
console command, which downloads the real prebuilt binary from
[GitHub Releases](https://github.com/kaorii-ako/whyslow/releases) on first
run, caches it under `~/.cache/whyslow/`, and execs it. No Rust toolchain
needed on your machine.

See the [main repo](https://github.com/kaorii-ako/whyslow) for what whyslow
actually does.

**whyslow requires root or `CAP_BPF` to run** — it loads eBPF programs, which
the kernel restricts to privileged processes:

```
sudo whyslow run -- <command>
```
