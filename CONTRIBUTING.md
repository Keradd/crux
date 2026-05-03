# Contributing to CRUX

Thanks for your interest in CRUX. This document covers the conventions
and the PR checklist that keep the workspace tidy and the build green.

## Code of conduct

Be civil. Argue ideas, not people. We follow the spirit of the
[Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct).

## Getting started

```bash
git clone https://github.com/Keradd/crux.git
cd crux
cargo build                  # warning-free in <5s warm
cargo test                   # 289 passing / 0 failed (default features)
cargo test --features crux-l7-sandbox/seccomp   # 299 passing / 0 failed (Linux)
```

Optional Linux extras:
- Kernel ≥ 3.5 for the `seccomp` feature.
- `landlock` is gated by Cargo feature on `crux-l7-sandbox`.

## Workspace conventions

These are observed patterns; please respect them when extending.

### Errors
- Every layer crate returns `crux_core::error::Result<T>` using the
  `CruxError` enum from `crux-core/src/error.rs`.
- Use `CruxError::other(...)` / `CruxError::Io { path, source }` etc.
- Do **not** invent new error types per crate. Add a variant to
  `CruxError` if absolutely necessary.

### Migrations
- Numbered `NNN_purpose.sql` in `crates/crux-core/migrations/`.
- Append a new entry to the `MIGRATIONS` array in
  `crates/crux-core/src/db.rs`.
- **Never edit a shipped migration.** Always add a new one.

### Types
- Every public domain type lives in a `types.rs` module so `lib.rs`
  can re-export with `pub use`.

### Tests
- In-module `#[cfg(test)] mod tests`.
- Use `tempfile::tempdir` for filesystem fixtures.
- Use `crux_core::db::open_in_memory` for DB fixtures (auto-applies
  migrations).
- Inline TOML goldens for L3 filters live under `[[tests]]` in
  `crates/crux-l3-bash/filters/*.toml` and run via the standard
  `cargo test` invocation.

### CLI commands
- One file per top-level command in `crates/crux-cli/src/commands/`.
- Args via `clap::Args`.
- Use `super::resolve_project_root` to find the project consistently.

### JSON output
- Every `crux ... --json` should emit a single JSON object/array via
  `serde_json::to_string_pretty`.
- Never mix human and JSON output on the same code path.

### Telemetry
- Record events via `crux_core::telemetry::record(&conn, &Event { … })`.
- Layer name is `"l1".."l10"` (lowercase). Feature is free-form
  `"layer:detail"`.

### Configuration
- Project config at `<root>/.crux/config.toml` overrides global
  `$CRUX_HOME/config.toml`.
- Defaults baked into `crux-core/src/config.rs::Default`.

### Dependencies
- Workspace deps live in the root `Cargo.toml`'s
  `[workspace.dependencies]` table.
- Per-crate `Cargo.toml` does `crux-core.workspace = true` etc.
- **No new dependencies without good reason.** Prefer the standard
  library or an existing workspace dep.

## Pull request checklist

Before opening a PR, please verify each of the following:

- [ ] `cargo fmt --all` is clean.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` is clean.
- [ ] `cargo test` passes (`cargo test --features crux-l7-sandbox/seccomp`
      on Linux if you touched L7).
- [ ] New behaviour is covered by tests.
- [ ] Public APIs include doc comments.
- [ ] If you added or changed a migration, the schema is documented in
      `docs/ARCHITECTURE.md` and the migration is appended (not edited
      in place).
- [ ] If you added a new CLI command or MCP tool, it is reflected in
      `README.md` and `CHANGELOG.md`.
- [ ] If you added a new layer or sub-stage, `CHANGELOG.md` (under
      `[Unreleased]`) reflects it.

## Filing issues

When filing a bug report, please include:
- CRUX version (`crux version`).
- OS + kernel version (`uname -a`).
- Rust toolchain version (`rustc --version`).
- A minimal reproduction (the smaller, the faster the fix).

For feature requests, please describe the use case rather than the
proposed implementation; the maintainers will help find the cleanest fit.

## License

By contributing, you agree that your contributions will be dual-licensed
under the terms of the [MIT license](LICENSE-MIT) and the
[Apache License 2.0](LICENSE-APACHE), without any additional terms or
conditions.
