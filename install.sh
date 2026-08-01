#!/bin/sh
# whyslow installer. Usage:
#   curl -sSf https://raw.githubusercontent.com/kaorii-ako/whyslow/main/install.sh | sh
#
# Downloads a prebuilt whyslow binary from GitHub Releases for this
# machine's OS/arch, verifies its checksum, and installs it.
#
# Env vars:
#   WHYSLOW_VERSION   release tag to install (default: latest)
#   WHYSLOW_INSTALL_DIR  install directory (default: ~/.local/bin, or
#                         /usr/local/bin if running as root)

set -eu

REPO="kaorii-ako/whyslow"

say() { printf '%s\n' "$1"; }
err() { printf 'whyslow install: error: %s\n' "$1" >&2; exit 1; }

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    err "this installer needs '$1', which was not found on PATH"
  fi
}

need_cmd curl
need_cmd tar
need_cmd sha256sum
need_cmd uname
need_cmd mktemp

os="$(uname -s)"
case "$os" in
  Linux) : ;;
  *) err "whyslow only supports Linux (it needs eBPF); detected: $os" ;;
esac

arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) target="aarch64-unknown-linux-gnu" ;;
  *) err "unsupported architecture: $arch (whyslow ships x86_64 and aarch64 builds)" ;;
esac

version="${WHYSLOW_VERSION:-}"
if [ -z "$version" ]; then
  say "Looking up latest whyslow release..."
  version="$(curl -sSf "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name":' | head -n1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
  [ -n "$version" ] || err "couldn't determine the latest release; set WHYSLOW_VERSION=vX.Y.Z and retry"
fi

asset="whyslow-${version}-${target}.tar.gz"
base_url="https://github.com/${REPO}/releases/download/${version}"

if [ -n "${WHYSLOW_INSTALL_DIR:-}" ]; then
  install_dir="$WHYSLOW_INSTALL_DIR"
elif [ "$(id -u)" = "0" ]; then
  install_dir="/usr/local/bin"
else
  install_dir="$HOME/.local/bin"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

say "Downloading ${asset} (${version}, ${target})..."
curl -sSfL "${base_url}/${asset}" -o "${tmp_dir}/${asset}" \
  || err "download failed -- does release ${version} have a ${target} build? see https://github.com/${REPO}/releases"
curl -sSfL "${base_url}/${asset}.sha256" -o "${tmp_dir}/${asset}.sha256" \
  || err "checksum download failed"

say "Verifying checksum..."
( cd "$tmp_dir" && sha256sum -c "${asset}.sha256" >/dev/null ) \
  || err "checksum verification failed -- downloaded file may be corrupt or tampered with"

say "Extracting..."
tar xzf "${tmp_dir}/${asset}" -C "$tmp_dir"

mkdir -p "$install_dir"
extracted_dir="${tmp_dir}/whyslow-${version}-${target}"
install -m 0755 "${extracted_dir}/whyslow" "${install_dir}/whyslow"

say ""
say "whyslow installed to ${install_dir}/whyslow"
case ":$PATH:" in
  *":${install_dir}:"*) : ;;
  *) say "NOTE: ${install_dir} isn't on your PATH. Add it, e.g.: export PATH=\"${install_dir}:\$PATH\"" ;;
esac
say ""
say "IMPORTANT: whyslow requires root or CAP_BPF to run -- it loads eBPF"
say "programs, which the kernel restricts to privileged processes. Run it"
say "with sudo. This will fail in most containers (need --cap-add=BPF"
say "--cap-add=PERFMON or --privileged), some restricted cloud shells, and"
say "WSL1 (WSL2 with a CAP_BPF/BTF-capable kernel works)."
say ""
say "Try: sudo whyslow run -- echo hello"
