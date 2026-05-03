# L7 Seccomp Implementation Plan

## Goal
Add seccomp syscall filtering to L7 hard isolation, with per-runtime allow-lists for Python, Bash, and Node.

## Decisions
- **Feature gating**: Separate cargo feature `seccomp` (like landlock)
- **Blocked syscall behavior**: `SIGSYS` trap (softer, easier debugging)
- **Allowlist strategy**: Restrictive start — only allow syscalls we KNOW interpreters need

## Architecture

### 1. New file: `src/seccomp.rs`

Contains:
- `ALLOWED_SYSCALLS_COMMON` — syscalls needed by all interpreters
- `ALLOWED_SYSCALLS_PYTHON` — Python-specific additions
- `ALLOWED_SYSCALLS_BASH` — Bash-specific additions  
- `ALLOWED_SYSCALLS_NODE` — Node-specific additions
- `BLOCKED_SYSCALLS` — always denied (ptrace, mount, etc.)
- `install_seccomp_filter(runtime: RuntimeKind) -> io::Result<()>` — builds and installs BPF filter

### 2. Syscall allowlists

**Common (all interpreters):**
- File I/O: read, write, open, close, stat, fstat, lstat, lseek, access, readlink, getdents64
- Memory: mmap, mprotect, munmap, brk, mremap
- Process: exit_group, exit, getpid, getppid, getuid, getgid, geteuid, getegid
- Signals: rt_sigaction, rt_sigprocmask, rt_sigreturn, sigaltstack
- Time: clock_gettime, clock_getres, gettimeofday
- Misc: ioctl, fcntl, pipe, dup, dup2, select, sched_yield

**Python additions:**
- openat, newfstatat, faccessat (modern glibc path)
- getrandom (for entropy)
- statx (newer stat syscall)
- clone (for threading, but limited)
- wait4 (for child processes)

**Bash additions:**
- pipe2, dup3
- execve (for running commands)
- setpgid, tcgetattr, tcsetattr (job control)
- wait4, waitid

**Node additions:**
- epoll_create1, epoll_ctl, epoll_wait
- eventfd2, timerfd_create, timerfd_settime, timerfd_gettime
- signalfd4
- inotify_init1, inotify_add_watch (file watching)
- socket, connect, bind, listen, accept (network)
- sendto, recvfrom, sendmsg, recvmsg

**Always blocked (dangerous):**
- ptrace (debugging/anti-debug)
- kexec_load, reboot, init_module, etc. (kernel module loading)
- mount, umount2 (filesystem mounting)
- swapon, swapoff
- sethostname, setdomainname

### 3. Implementation approach

Use raw BPF via `libc::prctl` + `libc::seccomp`:

```rust
fn install_seccomp_filter(runtime: RuntimeKind) -> io::Result<()> {
    // 1. PR_SET_NO_NEW_PRIVS (required for unprivileged seccomp)
    prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    
    // 2. Build BPF program
    //    - Load syscall number into accumulator
    //    - If in allowed set → ALLOW
    //    - If in blocked set → TRAP (SIGSYS)
    //    - Default: TRAP (restrictive strategy)
    
    // 3. Install via seccomp() syscall
    seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_LOG, &prog);
}
```

### 4. Integration point

In `executor.rs`, `linux::install()`:
```rust
#[cfg(feature = "seccomp")]
{
    if let Err(e) = apply_seccomp(req.runtime) {
        tracing::warn!(error = %e, "seccomp filter not applied");
    }
    applied.push("seccomp".to_string());
}
```

### 5. Cargo.toml changes

```toml
[features]
seccomp = []  # No extra deps, uses libc directly

[target.'cfg(target_os = "linux")'.dependencies]
# seccomp uses libc directly, no external crate needed
```

### 6. Test strategy

- `seccomp_applied_label_present` — verify "seccomp" in isolation_applied
- `seccomp_python_runs_hello` — Python print works under seccomp
- `seccomp_bash_runs_echo` — Bash echo works under seccomp
- `seccomp_blocks_ptrace` — ptrace syscall is blocked (python script trying to debug itself gets SIGSYS)
- `seccomp_graceful_degrade` — on non-Linux, seccomp silently skipped

### 7. Risk mitigations

- **Overly restrictive**: Start permissive, block only known-dangerous syscalls. Use SIGSYS trap instead of hard kill for easier debugging during development.
- **Platform-specific**: All seccomp code behind `#[cfg(target_os = "linux")]` and `#[cfg(feature = "seccomp")]`
- **Fallback**: seccomp is advisory — if it fails, log warning and continue (like landlock)

## Files to modify

1. `Cargo.toml` — add `seccomp` feature (no new deps)
2. `src/seccomp.rs` — NEW: syscall lists + filter installation
3. `src/lib.rs` — add `mod seccomp;`
4. `src/executor.rs` — integrate seccomp in `linux::install()`
5. `src/types.rs` — no changes needed (IsolationLevel::Hard already exists)
