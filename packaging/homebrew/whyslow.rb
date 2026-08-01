# Homebrew formula for whyslow.
#
# This file lives in the main whyslow repo but is meant to be copied (or
# CI-synced) into a separate `homebrew-whyslow` tap repo -- Homebrew requires
# formulas for `brew install <user>/<tap>/<formula>` to live in a repo named
# `homebrew-<tap>`, which only the repo owner can create (see DISTRIBUTION.md).
#
# whyslow is Linux-only (it loads eBPF programs), so this formula only
# installs a prebuilt binary on Linux and refuses on macOS.
class Whyslow < Formula
  desc "Debug why a Linux process was slow via eBPF causal-chain tracing"
  homepage "https://github.com/kaorii-ako/whyslow"
  version "0.1.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    odie "whyslow needs eBPF and only runs on Linux."
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/kaorii-ako/whyslow/releases/download/v0.1.0/whyslow-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9ff66cd8211c014b477088c88636d76f8bb33661cb44910bd402fcf50dc5c7cb"
    elsif Hardware::CPU.arm?
      url "https://github.com/kaorii-ako/whyslow/releases/download/v0.1.0/whyslow-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a1d714d0afdc5b46b9d5d065dd880403d7044e5bd0a3f90c8cb8c79316236cc7"
    else
      odie "whyslow has no prebuilt binary for #{Hardware::CPU.arch}."
    end
  end

  def install
    bin.install "whyslow"
  end

  def caveats
    <<~EOS
      whyslow requires root or CAP_BPF to run -- it loads eBPF programs,
      which the kernel restricts to privileged processes:

        sudo whyslow run -- <command>

      This will fail in most containers (need --cap-add=BPF --cap-add=PERFMON
      or --privileged), some restricted cloud shells, and WSL1 (WSL2 with a
      CAP_BPF/BTF-capable kernel works).
    EOS
  end

  test do
    assert_match "requires root", shell_output("#{bin}/whyslow run -- true 2>&1", 1)
  end
end
