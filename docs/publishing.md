# Publishing sinter

How sinter reaches each distribution channel. GitHub releases are fully
automated (tag push → `release.yml`); everything below is either automated
off the release or a short manual command list.

Versioning stays single-source: `make bump VERSION=x.y.z` rewrites both the
`[workspace.package]` version and the `sinter-* = { version = "…" }` path-dep
versions in the root `Cargo.toml` (path deps need an explicit version for
crates.io; cargo does not support workspace-inherited dependency versions).

## crates.io

### One-time setup

- [x] All five names (`sinter-core`, `sinter-store`, `sinter-extract`,
      `sinter-resolve`, `sinter-io`) are claimed and published (0.38.0).
- Per session: `cargo login` with a fresh crates.io publish-scoped token;
  revoke it afterwards. The account needs a verified email.

### Per release

Publish in dependency order; each step must be on crates.io before the next
resolves (crates.io indexes within seconds, `cargo publish` retries briefly
on its own):

```sh
cargo publish -p sinter-core
cargo publish -p sinter-store
cargo publish -p sinter-extract
cargo publish -p sinter-resolve
cargo publish -p sinter-io
```

Smoke test first with `cargo publish --dry-run -p sinter-core`. Dry runs of
the dependent crates fail until sinter-core exists on crates.io — expected.

`sinter-io` carries `[package.metadata.binstall]`, so once it is on
crates.io, `cargo binstall sinter-io` installs the prebuilt release binary
from the versioned GitHub asset URLs.

## PyPI (maturin wheels)

`.github/workflows/pypi.yml` builds wheels for the six release targets (plus manylinux retags of the
musllinux wheels — the static binary is valid under both linux tags, and
glibc pip/uv reject musllinux; `skip-existing` keeps reruns green) from
the `sinter-io` bin crate (directory crates/sinter-cli) (`maturin`, `bindings = "bin"`,
`crates/sinter-cli/pyproject.toml`) whenever a GitHub release is published,
then uploads via Trusted Publishing. It can also be run manually via
workflow_dispatch.

### One-time setup (done for sinter-io, 0.38.0)

- [x] Package name `sinter-io` — lives in exactly one place:
      `crates/sinter-cli/pyproject.toml` `[project] name`.
- [x] PyPI Trusted Publisher: owner `shellfu`, repository `sinter`,
      workflow `pypi.yml`, environment `pypi`.
- [x] GitHub `pypi` environment (Settings → Environments).

### Per release

Nothing — publishing a GitHub release triggers the workflow.

## Homebrew tap

Formula source of truth: `packaging/homebrew/sinter.rb` (versioned release
tarballs per OS/arch, sha256 pinned).

### One-time setup

- [x] `shellfu/homebrew-tap` exists with `Formula/sinter.rb` at 0.38.0.

### Per release

```sh
packaging/homebrew/update-formula.sh 0.38.0   # fills version + sha256s
cp packaging/homebrew/sinter.rb ../homebrew-tap/Formula/sinter.rb
# commit + push in the tap repo
```

Users then install with:

```sh
brew install shellfu/tap/sinter
```

## Release attestation

`release.yml` attests build provenance for every `sinter-*` asset
(`actions/attest-build-provenance`). Anyone can verify a download:

```sh
gh attestation verify sinter-<target>.tar.gz --owner shellfu
```
