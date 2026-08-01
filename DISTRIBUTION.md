# Distribution

Status of each channel from the original task brief, what's automated vs.
what needs a one-time manual step only the repo/account owner can do.

## 1. crates.io

**Status: partially live, and the "cargo install whyslow" headline goal
turned out not to work — found out by actually trying it, not by reasoning
about it in advance.**

`whyslow-common` ([crates.io](https://crates.io/crates/whyslow-common)) and
`whyslow-ebpf` ([crates.io](https://crates.io/crates/whyslow-ebpf)) *are*
published, real, live, both at 0.1.1. `.github/workflows/publish-crates.yml`
publishes them automatically on every `vX.Y.Z` tag push (or manually via
`gh workflow run "Publish to crates.io"`), each step idempotent (skips if
that version's already up).

**`whyslow` (the CLI) is deliberately not published — `publish = false` in
its Cargo.toml — because `cargo install whyslow` from the registry doesn't
work with the current build.rs, confirmed by actually attempting the publish
and hitting the real failure:**

```
error: failed to run custom build command for `whyslow v0.1.1`
Error: whyslow-ebpf package not found
```

The root cause: `whyslow`'s `build.rs` (the standard `aya-template` pattern)
finds `whyslow-ebpf` via `cargo_metadata::MetadataCommand::new().no_deps()`
— a *workspace-member* lookup. That only resolves `whyslow-ebpf` when it's
an actual sibling in the same workspace, which is true when the whole repo
is built from a git clone (our normal build path, and every CI/Release build
here), but **not** true when `whyslow-ebpf` is instead a plain registry
dependency, as it necessarily is during a real `cargo install whyslow` from
crates.io. This isn't hypothetical — I hit it by actually running the
publish and reading the resulting error, then verified the same design
works fine via `cargo install --git`, isolating the cause to exactly this.

**The real working equivalent:**
```
cargo install --git https://github.com/kaorii-ako/whyslow whyslow
```
This works today (verified end-to-end: installs, runs, produces the correct
`explain` output) because a git install clones the actual repo, preserving
the workspace `whyslow-ebpf` needs to be found in.

Either way — registry or git — installing this way still needs *your*
machine to have a nightly toolchain + `rust-src` + `bpf-linker` (+ its LLVM
dev libs), because the build compiles `whyslow-ebpf` from source. That part
really is inherent to how `aya`-based tools build, not fixable here. Channels
2-6 below exist specifically so end users don't have to hit either issue.

**Needs you** (for the two crates that *are* publishable): a crates.io
account + `CARGO_REGISTRY_TOKEN` secret, already done as of this writing.

**Follow-up, not done here:** rewriting `whyslow`'s `build.rs` to not depend
on workspace-member resolution (e.g. vendoring `whyslow-ebpf`'s source into
`OUT_DIR` and building it there directly, or using `cargo_metadata` without
`.no_deps()` and resolving the dependency's actual manifest path instead of
its workspace membership) would fix this properly and let `whyslow` publish
too. Left for later since it's a real (if bounded) rewrite, not a
config tweak — flagging it here rather than either silently shipping a
broken package or quietly giving up on the channel.

## 2. GitHub Releases + install script

**Fully automated**, no credentials needed beyond what GitHub Actions
already provides automatically (`GITHUB_TOKEN`). `.github/workflows/release.yml`,
on every `vX.Y.Z` tag: builds native x86_64 and aarch64 binaries (real
hardware via `ubuntu-24.04-arm`, not cross-compiled/emulated), packages
them as `whyslow-vX.Y.Z-<target>.tar.gz` + `.sha256`, and publishes a
GitHub Release. `install.sh` detects arch, downloads, verifies the
checksum, and installs. Validated end-to-end against the real v0.1.0 release.

aarch64 has `continue-on-error: true` at the job level in case hosted arm64
runners aren't available on some account tier in the future — x86_64 release
won't be blocked by that.

## 3. Homebrew tap

**Done, live**: https://github.com/kaorii-ako/homebrew-whyslow (a real repo,
created and pushed to as part of this work — Homebrew requires
`homebrew-<name>` as the exact repo name, which only an account owner can
create, but since this account authorized it, it's set up for real).
`brew install kaorii-ako/whyslow/whyslow` works today against the v0.1.0
release. The formula (`packaging/homebrew/whyslow.rb`, kept in the main repo
too as the source of truth) has real sha256 checksums, not placeholders.

**Needs you, going forward:** every new release needs the formula's
`version`/`url`/`sha256` fields updated and pushed to the tap repo. Not
automated yet -- doing so would need a CI job in *this* repo with push
access to the *tap* repo, i.e. a personal access token scoped to it, stored
as a secret. Worth adding once there's a second release to prove out; skipped
for v1 to avoid over-building before there's a real update to test against.

## 4. Own APT repository

**Automated, but unsigned.** `.github/workflows/release.yml`'s `apt-repo` job
builds a `.deb` per architecture via `cargo-deb` (config in
`whyslow-cli/Cargo.toml`'s `[package.metadata.deb]`), assembles a flat
repository (`dpkg-scanpackages` + `Packages.gz`, no `pool`/`dists` structure
needed for this scale) and deploys it to GitHub Pages
(https://kaorii-ako.github.io/whyslow/apt/) via `actions/deploy-pages`. Pages
itself is already enabled (GitHub Actions build source) — that part needed
one API call, done.

```
echo "deb [trusted=yes] https://kaorii-ako.github.io/whyslow/apt ./" \
  | sudo tee /etc/apt/sources.list.d/whyslow.list
sudo apt update && sudo apt install whyslow
```

**Needs you, to remove the `[trusted=yes]`/unsigned caveat:** generate a GPG
keypair, add the private key + passphrase as repo secrets, sign `Packages`
into an `InRelease`/`Release.gpg`, and publish the public key at a stable URL
so `apt-add-repository`'s `signed-by` mechanism can verify it. This requires
a keypair only you can create/own, so it's left as a documented follow-up
rather than something faked with a throwaway key.

## 5. AUR

`packaging/aur/PKGBUILD` is written (a `-bin` package pulling the prebuilt
GitHub Release binary, with real checksums — building `whyslow-ebpf` from
source via `makepkg` would require nightly + bpf-linker on every installer's
machine, the same real cost noted in the crates.io section, so a binary
package is the right call here specifically).

**Needs you:** AUR requires its own account and an SSH key registered with
it, then `git push` to `ssh://aur@aur.archlinux.org/whyslow-bin.git` — there
is no way to automate the *first* push without your AUR credentials. Once
that initial repo exists, updating it on new releases could be automated
(same shape as the Homebrew tap: a CI job with an SSH deploy key secret) —
also deferred until there's a second release to prove the update path
against.

## 6. PyPI wrapper

**Package fully written and tested end-to-end** against the real v0.1.0
release: `pip install <path>` → first `whyslow` invocation downloads the
matching prebuilt binary from GitHub Releases into `~/.cache/whyslow/<ver>/`,
verifies its checksum, `exec`s it; second invocation reuses the cache with no
network call. Pure-stdlib (`urllib`, `hashlib`, `tarfile`), no build step
needed on the user's machine, works on any platform pip supports (fails
loudly with a clear message on non-Linux, since whyslow itself is Linux-only).

**Needs you:**
1. Create a PyPI account, generate an API token
   (https://pypi.org/manage/account/#api-tokens).
2. Either `twine upload` manually from `packaging/pypi/` after
   `python -m build`, or add `PYPI_API_TOKEN` as a repo secret and I can wire
   up a `publish-pypi.yml` workflow the same way as crates.io's -- deferred
   for the same reason as the tap update automation: no second release yet to
   validate the update path against, and every account/token step here is one
   only you can do regardless.

## Summary table

| Channel | Automated | Needs from you |
|---|---|---|
| crates.io | `whyslow-common`/`whyslow-ebpf` live; `whyslow` CLI intentionally unpublished (see above) | done for the two publishable crates |
| GitHub Releases | fully automated, validated live | nothing |
| Homebrew | tap repo live, real checksums | nothing to start; future auto-update needs a tap-repo PAT |
| APT (unsigned) | fully automated, validated live | nothing to start; GPG signing needs a keypair only you can create |
| AUR | PKGBUILD written | AUR account + SSH key, manual first push |
| PyPI | package written, tested live | PyPI account + token to actually publish |

Everywhere: **whyslow needs root or `CAP_BPF` to run, regardless of install
method** — every installer/README/wrapper above says so explicitly, so a
"permission denied" isn't a surprise.
