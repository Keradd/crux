## Description

<!-- Briefly describe what this PR does and why. Link the issue if applicable. -->

Fixes #

## Checklist

- [ ] `cargo fmt --all -- --check` is clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo test --workspace --exclude crux-l7-sandbox` passes
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` is clean
- [ ] Commits follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] New behaviour is covered by tests
- [ ] Public APIs include doc comments; new CLI flags documented in `--help`
- [ ] If migration added: schema documented in `docs/ARCHITECTURE.md` §5, migration appended (not edited in place)
- [ ] If CLI command or MCP tool added: reflected in `README.md`, `docs/ARCHITECTURE.md`, and `CHANGELOG.md`

## Test output

```
Paste `cargo test` summary here (relevant section)
```
