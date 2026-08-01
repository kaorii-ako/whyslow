# Distribution

Status of each channel from the original task brief, what's automated vs.
what needs a one-time manual step only the repo/account owner can do.

## 1. crates.io — `cargo install whyslow`

**Automated:** `.github/workflows/publish-crates.yml` publishes
`whyslow-common` → `whyslow-ebpf` → `whyslow` (in dependency order, with a
30s pause between each for crates.io's index to catch up) on every `vX.Y.Z`
tag push. All three crates already have the metadata crates.io requires
(description, license, repository, keywords/categories), and the path
dependencies between them carry explicit versions so `cargo publish` can
resolve them.

**Needs you:**
1. Create a crates.io account (via GitHub OAuth is fastest) at
   https://crates.io.
2. Generate an API token: https://crates.io/settings/tokens.
3. Add it as a repo secret: `gh secret set CARGO_REGISTRY_TOKEN` (or
   Settings → Secrets and variables → Actions on GitHub.com).
4. The workflow silently no-ops on every tag push until that secret exists,
   then runs automatically on the next tag.

**Real, unavoidable limitation:** `cargo install whyslow` on someone else's
machine still needs *them* to have a nightly toolchain + `rust-src` +
`bpf-linker` (+ its LLVM dev libs) installed locally, because
`whyslow`'s `build.rs` compiles `whyslow-ebpf` from source at install time.
This isn't a bug or something we can paper over — it's inherent to how
`aya`-based tools currently distribute via cargo. Channels 2-6 below exist
specifically so end users don't have to hit this.

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
| crates.io | publish workflow ready | crates.io account + `CARGO_REGISTRY_TOKEN` secret |
| GitHub Releases | fully automated, validated live | nothing |
| Homebrew | tap repo live, real checksums | nothing to start; future auto-update needs a tap-repo PAT |
| APT (unsigned) | fully automated, validated live | nothing to start; GPG signing needs a keypair only you can create |
| AUR | PKGBUILD written | AUR account + SSH key, manual first push |
| PyPI | package written, tested live | PyPI account + token to actually publish |

Everywhere: **whyslow needs root or `CAP_BPF` to run, regardless of install
method** — every installer/README/wrapper above says so explicitly, so a
"permission denied" isn't a surprise.
