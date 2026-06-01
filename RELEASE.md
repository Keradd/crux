# Release process

CRUX uses [cargo-release](https://github.com/crate-ci/cargo-release) for
version bumps and crates.io publishing. Binary builds are **not shipped** —
users install via `cargo install crux`.

## Steps

### 1. Prepare the release branch

```bash
git checkout -b release/v0.4.1
```

### 2. Update version and changelog

```bash
# Bump version across all workspace crates
cargo release --workspace --execute 0.4.1
```

Then edit `CHANGELOG.md`:

- Move `[Unreleased]` entries under the new version header.
- Add a new empty `[Unreleased]` section at the top.

### 3. Commit and tag

```bash
git add -A
git commit -m "release: v0.4.1"
git tag -a v0.4.1 -m "v0.4.1"
```

### 4. Publish to crates.io

Publish crates in dependency order (each `cargo publish` must succeed
before the next):

```bash
# Foundation
cargo publish -p crux-core

# Layers (order does not matter among themselves)
cargo publish -p crux-l3-bash
cargo publish -p crux-l4-readcache
cargo publish -p crux-l5-ast
cargo publish -p crux-l6-search
cargo publish -p crux-l7-sandbox
cargo publish -p crux-l8-memory
cargo publish -p crux-l9-coach
cargo publish -p crux-l10-setup
cargo publish -p crux-l11-digest
cargo publish -p crux-l12-hygiene
cargo publish -p crux-humanizer
cargo publish -p crux-mcp

# Binary (last — depends on all above)
cargo publish -p crux-cli
```

After the last publish, `cargo install crux` will resolve the new version.

### 5. GitHub release

```bash
git push origin main --tags
```

Then create a release on GitHub:

1. Go to https://github.com/Keradd/crux/releases/new
2. Select the tag (e.g. `v0.4.1`)
3. Title: `v0.4.1`
4. Description: paste the CHANGELOG entry for this version
5. Publish — no binary assets to upload

### 6. Verify

```bash
cargo install crux
crux --version      # should show the new version
```

## Pre-release checklist

- [ ] `cargo test --workspace --exclude crux-l7-sandbox` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all -- --check` is clean
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` is clean
- [ ] `CHANGELOG.md` reflects all changes since last release
- [ ] Version bumped in root `Cargo.toml` (`[workspace.package].version`)
- [ ] Version consistency: all internal `[workspace.dependencies]` entries match
