# CRUX Comment Hygiene / Slop Guard

`crux hygiene comments` is a deterministic scanner, auto-fixer,
and stripper for AI-flavoured comments in source code. It flags
decorative banners, long `//!` module docs, `Goal:` /
`Public surface:` blocks, `Pattern adapted from …` references,
`Layer N` labels, and marketing fluff (`revolutionary`,
`cutting-edge`, `seamlessly`, `robust and scalable`, …).

The rewrite is **local and deterministic** — no LLM, no network —
so the same input always produces the same output and is safe to
run from CI hooks.

## Manual usage

```bash
crux hygiene comments --check          # scan; exit 1 on any violation
crux hygiene comments --fix            # apply auto-fix in place
crux hygiene comments --strip          # strip every non-essential comment
                                       # (preserves SAFETY, SECURITY, WARNING,
                                       # TODO/FIXME, and `///` doctests)
crux hygiene comments --check --json   # machine-readable report
crux hygiene comments --check --path src/lib.rs --path src/main.rs
                                       # scan only the given files
```

The check tool walks the project root (`--root` overrides), reads
Rust / TOML / Markdown / YAML / JS / TS / Python files, and skips
generated artefacts (`@generated` / "do not edit" headers,
`target/`, `.git/`, `node_modules/`, `Cargo.lock`,
`package-lock.json`, …).

## Build usage

```bash
crux build                             # hygiene check + `cargo build` if clean
crux build --skip-hygiene -- --release # escape hatch + extra cargo args
```

`crux build` is a thin wrapper: runs the same scan, aborts the
build on any violation, then hands off to `cargo build` with every
argument after `--` passed through verbatim. Use `--skip-hygiene`
when you need a build without running the guard.

## Agent hook usage

Agents that support hooks (currently **Claude Code**) can run the
hygiene check automatically after every Edit / Write / MultiEdit.
The hook is **opt-in** and **warn-only** — it never auto-rewrites
files.

```bash
# Register the hook alongside the regular CRUX setup.
crux setup claude-code --enable-hygiene-hook

# Remove it later.
crux setup claude-code --disable-hygiene-hook
```

This writes (or drops) a `PostToolUse` entry in
`~/.claude/settings.json` that runs:

```
crux hygiene comments --check --changed-from-stdin
```

The `--changed-from-stdin` flag makes the CLI read the Claude Code
PostToolUse JSON on stdin and scan **only the file that was just
edited**, instead of walking the whole repo. Tools other than
Edit / Write / MultiEdit / NotebookEdit are ignored (exit 0).

Behaviour:

- Exit 2 + violation report on stderr → Claude Code surfaces the
  warning to the model so the agent sees it and can decide to
  clean up before moving on. (Manual / CI invocations still exit 1
  on violation and print to stdout.)
- Exit 0 on clean files, unsupported tools, or empty payloads — so
  a missing file path never blocks a tool call.
- **No auto-fix.** Run `crux hygiene comments --fix` manually if
  you want the deterministic rewrite.

## What `--fix` does

- Drops decorative banner comments (`// ────────`, `# ====…`).
- Removes `Goal:` and `Public surface:` doc-comment blocks
  (header line + the bullet/blank lines that follow it).
- Compresses long Rust module-doc runs to a single short sentence
  (the first non-empty `//!` line in the run is kept).
- **Never** modifies code lines, fenced code blocks in markdown,
  `// SAFETY:` / `// SECURITY:` / `// WARNING:` / `// TODO:`
  comments, or any markdown source file.

## What `--strip` does

Aggressive pass: removes every `//`, `///`, and `//!` comment in
the workspace except the small protected set:

- `// SAFETY:` / `// SECURITY:` / `// WARNING:` / `// TODO:` /
  `// FIXME:`.
- `///` doctest blocks that contain a fenced ```` ``` ```` example.

Idempotent; collapses blank lines left behind; never touches
markdown source.

## What it does *not* fix

`marketing-phrase`, `pattern-adapted-from`, and `layer-label`
violations are reported but never auto-rewritten — those need
human judgement to keep the surrounding sentence meaningful. Run
`crux humanize --mode developer` on the file if you want a
deterministic prose rewrite as well.

## Extending

The rule tables live in `crates/crux-l12-hygiene/src/rules.rs` —
adding a new banner character or marketing phrase is a one-line
change with a co-located test.
