# Contributing to CRUX

Thanks for your interest in CRUX. This document covers the conventions
and the PR checklist that keep the workspace tidy and the build green.

## Contents

- [Code of conduct](#code-of-conduct)
- [Getting started](#getting-started)
- [Development workflow](#development-workflow)
- [Workspace conventions](#workspace-conventions)
- [Comment hygiene](#comment-hygiene)
- [Commit message conventions](#commit-message-conventions)
- [Pull request checklist](#pull-request-checklist)
- [Adding a new layer](#adding-a-new-layer)
- [Filing issues](#filing-issues)
- [Security](#security)
- [License](#license)

## Code of conduct

Be civil. Argue ideas, not people. We follow the spirit of the
[Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct).

## Getting started

MSRV is **Rust 1.85** (edition 2021). Rustup will pull the right
toolchain automatically because the workspace's `rust-version` is
pinned in `Cargo.toml`.

```bash
git clone https://github.com/Keradd/crux.git
cd crux
cargo build                  # warning-free in <5s warm
cargo test                   # 621 passing / 0 failed (default features)
cargo test --features crux-l7-sandbox/seccomp   # adds Linux-only seccomp tests
```

Optional Linux extras:
- Kernel ≥ 3.5 for the `seccomp` feature.
- `landlock` is gated by Cargo feature on `crux-l7-sandbox`.

## Development workflow

Typical contributor loop:

1. **Branch** — off `main`, named `feat/<slug>`, `fix/<slug>`,
   `docs/<slug>`, or `chore/<slug>`.
2. **Code** — keep diffs tight; favour minimal upstream fixes over
   downstream workarounds.
3. **Test** — write or extend tests *before* the implementation where
   practical. New behaviour without coverage will be asked for it.
4. **Lint** — the local equivalents of the four CI jobs:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
   ```
   Prefer `crux build` over raw `cargo build` — it runs the L12
   hygiene check first and aborts on AI-flavoured comments.
5. **Docs** — update `README.md`, `docs/ARCHITECTURE.md`, and / or the
   relevant crate rustdoc whenever public surface shifts.
6. **Changelog** — append an entry under `## [Unreleased]` in
   `CHANGELOG.md`. Group by `### Added / Changed / Fixed / Removed /
   Docs / Tests / Planned` as the file already does.
7. **Commit** — follow the [Conventional Commits](#commit-message-conventions)
   convention already used in the git log.
8. **PR** — link the issue (if any), paste the `cargo test` summary,
   and tick the [Pull request checklist](#pull-request-checklist).

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
- Layer name is `"l1".."l11"` (lowercase). Feature is free-form
  `"layer:detail"`.
- Every compression step must also emit its token counters so
  `crux audit` can attribute savings.

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

## Comment hygiene

CRUX prefers clear names and tests over explanatory comments.
The L12 Slop Guard (`crux hygiene comments`) is the tool we run
against ourselves — same rule set any contributor is expected to
meet before a PR is merged.

Rules for new code:

- **Default is zero comments.** Use descriptive names for
  functions, types, variables, and modules instead of explanatory
  prose.
- Allowed exceptions — only when there is a real engineering
  reason a reader would otherwise miss:
  - `// SAFETY:` above every `unsafe { ... }` block or
    `unsafe impl`.
  - `// SECURITY:` for credential / auth / sandbox-escape-relevant
    code.
  - `// WARNING:` for non-obvious footguns.
  - `// TODO:` / `// FIXME:` only when the work is tracked.
  - `///` doctests with runnable ````` examples.
  - One-line invariants the compiler cannot express.
- **Banned styles**: decorative banners (`// ────`, `# ====`),
  `//! Goal:` / `//! Public surface:` blocks, marketing phrases
  (`robust`, `cutting-edge`, …), `Layer N` duplicate labels,
  AI-style preambles ("This function does X by Y"), and obvious
  restatements (`// increment counter`).

Verify locally before opening a PR:

```bash
crux hygiene comments --check       # exit 1 on any violation
crux hygiene comments --fix         # auto-clean banners / Goal blocks
crux hygiene comments --strip       # aggressive: remove non-essential comments
crux build                          # hygiene check + cargo build in one step
```

`crux build` is the preferred build entry point for contributors
— it is `cargo build` with the hygiene gate in front, so the
review never has to relitigate comment style.

## Commit message conventions

CRUX uses [Conventional Commits](https://www.conventionalcommits.org/)
— same pattern you'll see in `git log`:

```
<type>(<scope>): <short summary, imperative mood>

<optional body wrapped to ~72 cols>

<optional footer, e.g. Fixes #42 / BREAKING CHANGE: …>
```

- `<type>` is one of **`feat`**, **`fix`**, **`docs`**, **`test`**,
  **`refactor`**, **`perf`**, **`chore`**, **`ci`**, **`build`**,
  **`revert`**, **`release`**.
- `<scope>` is usually a crate or layer: `l4`, `l11-digest`,
  `crux-mcp`, `ci`, `changelog`, `install.sh`, … Skip the scope for
  cross-cutting chores.
- Keep the summary under **72 characters**, lower-case except for
  proper nouns.
- Breaking changes get both a `!` after the type and a
  `BREAKING CHANGE:` footer.

Examples pulled from history:

```
feat(v0.2.0): crux setup 8 agents + init bootstrap chain + one-shot installer
fix(l7-sandbox): cross-libc rlimit type alias
fix(ci): green up matrix on Rust 1.95 + Windows + 1.85 MSRV
docs(changelog): finalize v0.2.0 — 8 agents, init chain, one-shot installer
release: v0.1.1 — fix L7 Python on Windows + musl cross-build
```

## Pull request checklist

Before opening a PR, please verify each of the following:

- [ ] `cargo fmt --all -- --check` is clean.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] `cargo test --workspace` passes
      (`cargo test --workspace --features crux-l7-sandbox/seccomp`
      on Linux if you touched L7).
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
      is clean.
- [ ] `crux hygiene comments --check` is clean (or the offending
      comments carry a documented `SAFETY` / `SECURITY` / `WARNING`
      / `TODO` reason).
- [ ] New behaviour is covered by tests.
- [ ] Public APIs include doc comments; any new CLI flag is documented
      in `--help` output (clap `#[arg(help = …)]`).
- [ ] If you added or changed a migration, the schema is documented in
      `docs/ARCHITECTURE.md` §5 and the migration is appended (not
      edited in place).
- [ ] If you added a new CLI command or MCP tool, it is reflected in
      `README.md`, `docs/ARCHITECTURE.md` §8/§9, and `CHANGELOG.md`.
- [ ] If you added a new layer or sub-stage, `CHANGELOG.md` (under
      `[Unreleased]`) reflects it and the [Adding a new layer](#adding-a-new-layer)
      checklist is satisfied.
- [ ] Commits follow the [Conventional Commits](#commit-message-conventions)
      convention.

## Adding a new layer

CRUX's power comes from its cross-layer integration, so a new layer
touches more places than a new flag. When you introduce `Lx`:

- [ ] Create `crates/crux-lx-<slug>/` with the standard `lib.rs` +
      `types.rs` + `engine.rs` split (mirror `crux-l11-digest` for the
      newest reference implementation).
- [ ] Add the crate to the workspace `members` list in the root
      `Cargo.toml` and wire it into `[workspace.dependencies]`.
- [ ] If the layer needs persistence, add a new numbered migration
      under `crates/crux-core/migrations/` and append it to the
      `MIGRATIONS` array in `crates/crux-core/src/db.rs`. Never edit
      a shipped migration.
- [ ] Expose a toggle in `crates/crux-core/src/config.rs` under
      `[layer.lx]` with sensible defaults; surface the toggle in
      `crux audit` / `crux_audit`.
- [ ] Add CLI subcommands under `crates/crux-cli/src/commands/` and MCP
      tools in `crates/crux-mcp/src/{tools.rs,dispatch.rs}`.
- [ ] Update `README.md` (Why-CRUX row, MCP tools table, workspace
      tree), `docs/ARCHITECTURE.md` (§3 diagram, §4 workspace, §7
      per-layer section, §8 MCP tools, §11 roadmap, §14.1 security
      row), and `CHANGELOG.md` under `[Unreleased]`.
- [ ] Teach `crux-l9-coach` about the new layer (`unused_layers` and
      the "Few layers active" threshold).
- [ ] Add crate-level tests; aim for the same density as the existing
      layer crates.

## Filing issues

When filing a bug report, please include:
- CRUX version (`crux version`).
- OS + kernel version (`uname -a`).
- Rust toolchain version (`rustc --version`).
- A minimal reproduction (the smaller, the faster the fix).

For feature requests, please describe the use case rather than the
proposed implementation; the maintainers will help find the cleanest fit.

## Security

CRUX handles local code, credentials, and AI-agent tool output, so
security regressions matter. Please do **not** file public issues for
suspected vulnerabilities. Instead:

1. Open a [private security advisory](https://github.com/Keradd/crux/security/advisories/new)
   on GitHub, or
2. Email the maintainers with a minimal reproduction and, if possible,
   a suggested patch.

The areas most worth scrutinising:

- L3 filter rules that might drop errors, credentials, or warnings
  (see `docs/ARCHITECTURE.md` §13.2 "Quality preservation").
- L7 sandbox escape paths — rlimits, landlock, seccomp allowlists.
- L4 read-cache serving a stale slice after an on-disk change.
- MCP stdio server parsing untrusted JSON-RPC frames.

The expected response time is **3 working days** for an acknowledgement
and a plan. CVEs will be coordinated through GitHub's advisory flow.

## License

By contributing, you agree that your contributions will be dual-licensed
under the terms of the [MIT license](LICENSE-MIT) and the
[Apache License 2.0](LICENSE-APACHE), without any additional terms or
conditions.
