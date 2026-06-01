# Architecture map

```
hello-crux/
├── Cargo.toml
├── CLAUDE.md
├── src/
│   └── main.rs
└── .crux/
    ├── config.toml
    ├── COMMON_MISTAKES.md
    ├── QUICK_START.md
    ├── ARCHITECTURE_MAP.md
    ├── contextignore
    ├── completions/
    ├── sessions/
    │   ├── active/
    │   └── archive/
    └── captures/
```

## Key files

- **`src/main.rs`** — single entry point, 5 commands: greet, add, stats, fibonacci, help
- **`CLAUDE.md`** — agent instructions
- **`.crux/config.toml`** — CRUX configuration
