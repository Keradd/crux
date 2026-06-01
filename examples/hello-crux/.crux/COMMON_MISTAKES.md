# Common mistakes

## Top 5

### 1. Off-by-one in fibonacci

- **Symptom**: `fib(0)` returns 0 instead of 1
- **Check**: base cases match 0→0, 1→1
- **Fix**: verify the sequence definition before changing

### 2. Forgot to rebuild before testing

- **Symptom**: old binary still runs
- **Check**: `cargo build` exit code
- **Fix**: always `cargo build` after changes

### 3. args index off by one

- **Symptom**: `greet` uses program name as name
- **Check**: `args.get(1)` is the first command argument
- **Fix**: verify indexing starting from `args[0]` (program name)

## Update this file when

- A bug took longer than 30 minutes to debug.
- A pattern violated Rust conventions.
