"""Thin wrapper: downloads the prebuilt `whyslow` binary from GitHub Releases
on first run, caches it, then execs it -- this package ships no Rust code of
its own. Modeled on how ruff/maturin-built tools distribute native binaries
via PyPI, except this fetches at first-run rather than bundling per-platform
wheels at build time (simpler CI, at the cost of needing network on first use).
"""

import hashlib
import os
import platform
import sys
import tarfile
import urllib.request

__version__ = "0.1.0"  # kept in sync with pyproject.toml's [project].version

REPO = "kaorii-ako/whyslow"
_ARCH_MAP = {
    "x86_64": "x86_64-unknown-linux-gnu",
    "amd64": "x86_64-unknown-linux-gnu",
    "aarch64": "aarch64-unknown-linux-gnu",
    "arm64": "aarch64-unknown-linux-gnu",
}


def _die(msg):
    sys.stderr.write(f"whyslow (pip wrapper): {msg}\n")
    sys.exit(1)


def _cache_dir(version):
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    return os.path.join(base, "whyslow", version)


def _target_triple():
    if platform.system() != "Linux":
        _die(f"whyslow only supports Linux (it needs eBPF); detected {platform.system()}")
    machine = platform.machine()
    triple = _ARCH_MAP.get(machine)
    if triple is None:
        _die(f"unsupported architecture: {machine} (whyslow ships x86_64 and aarch64 builds)")
    return triple


def _download(url, dest):
    try:
        with urllib.request.urlopen(url) as resp, open(dest, "wb") as f:
            f.write(resp.read())
    except OSError as e:
        _die(f"download failed ({url}): {e}")


def _sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _ensure_binary():
    version = os.environ.get("WHYSLOW_VERSION", f"v{__version__}")
    triple = _target_triple()
    cache_dir = _cache_dir(version)
    binary_path = os.path.join(cache_dir, "whyslow")

    if os.path.isfile(binary_path) and os.access(binary_path, os.X_OK):
        return binary_path

    os.makedirs(cache_dir, exist_ok=True)
    asset = f"whyslow-{version}-{triple}.tar.gz"
    base_url = f"https://github.com/{REPO}/releases/download/{version}"
    tarball = os.path.join(cache_dir, asset)
    checksum_file = tarball + ".sha256"

    sys.stderr.write(f"whyslow: downloading {asset} (first run only)...\n")
    _download(f"{base_url}/{asset}", tarball)
    _download(f"{base_url}/{asset}.sha256", checksum_file)

    with open(checksum_file) as f:
        expected = f.read().split()[0]
    actual = _sha256(tarball)
    if actual != expected:
        _die(f"checksum mismatch for {asset}: expected {expected}, got {actual}")

    with tarfile.open(tarball) as tf:
        member = next(m for m in tf.getmembers() if m.name.endswith("/whyslow"))
        member.name = "whyslow"
        tf.extract(member, cache_dir, filter="data")

    os.chmod(binary_path, 0o755)
    os.remove(tarball)
    os.remove(checksum_file)
    return binary_path


def main():
    binary_path = _ensure_binary()
    os.execv(binary_path, [binary_path] + sys.argv[1:])
