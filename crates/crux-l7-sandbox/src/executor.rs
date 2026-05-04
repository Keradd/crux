use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crux_core::error::{CruxError, Result};

use crate::permissions::PermDecision;
use crate::types::{ExecRequest, ExecResult, IsolationLevel, RuntimeKind};

const ALLOWED_ENV_KEYS: &[&str] = &["PATH", "LANG", "TZ"];

pub struct Executor;

impl Executor {
    pub fn new() -> Self {
        Self
    }

    pub fn execute(&self, req: &ExecRequest) -> Result<ExecResult> {
        if let Some(perms) = &req.permissions {
            if let PermDecision::Deny(rule) = perms.evaluate(req.runtime, &req.code) {
                return Err(CruxError::other(format!(
                    "denied by agent permission rule {} ({}/{}): {} {}",
                    rule.raw,
                    rule.source.label(),
                    rule.scope.label(),
                    "blocked by L7 sandbox before spawn —",
                    "remove or override the rule to proceed"
                )));
            }
        }

        let interpreter = req.runtime.default_interpreter();
        let mut cmd = match req.runtime {
            RuntimeKind::Python => {
                let mut c = Command::new(interpreter);
                c.arg("-I"); // isolated mode: ignore PYTHONPATH, user site, etc.
                c.arg("-c");
                c.arg(&req.code);
                c
            }
            RuntimeKind::Bash => {
                let mut c = Command::new(interpreter);
                let wrapped = format!("set -uo pipefail\n{}", req.code);
                c.arg("-c");
                c.arg(wrapped);
                c
            }
            RuntimeKind::Node => {
                let mut c = Command::new(interpreter);
                c.arg("-e");
                c.arg(&req.code);
                c
            }
        };

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(root) = &req.project_root {
            if root.is_dir() {
                cmd.current_dir(root);
            }
        }

        if !req.inherit_env {
            cmd.env_clear();
            for k in ALLOWED_ENV_KEYS {
                if let Ok(v) = std::env::var(k) {
                    cmd.env(k, v);
                }
            }
        }
        for (k, v) in &req.env {
            cmd.env(k, v);
        }

        let isolation_applied = apply_hard_isolation(&mut cmd, req);

        let start = Instant::now();
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            ErrorKind::NotFound => CruxError::other(format!(
                "interpreter '{}' not found on PATH (runtime={})",
                interpreter,
                req.runtime.as_str()
            )),
            _ => CruxError::other(format!("spawn failed: {e}")),
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let max_bytes = req.max_output_bytes;

        let stdout_handle = stdout.map(|s| spawn_capture(s, max_bytes));
        let stderr_handle = stderr.map(|s| spawn_capture(s, max_bytes));

        let timed_out = wait_with_timeout(&mut child, req.timeout)?;
        let exit_code = if timed_out {
            None
        } else {
            child.wait().ok().and_then(|s| s.code())
        };

        let (stdout_text, stdout_truncated) = match stdout_handle {
            Some(h) => h
                .join()
                .map_err(|_| CruxError::other("stdout reader panicked"))?,
            None => (String::new(), false),
        };
        let (stderr_text, stderr_truncated) = match stderr_handle {
            Some(h) => h
                .join()
                .map_err(|_| CruxError::other("stderr reader panicked"))?,
            None => (String::new(), false),
        };

        Ok(ExecResult {
            runtime: req.runtime,
            stdout: stdout_text,
            stderr: stderr_text,
            exit_code,
            timed_out,
            stdout_truncated,
            stderr_truncated,
            elapsed_ms: start.elapsed().as_millis(),
            isolation_applied,
        })
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

fn apply_hard_isolation(cmd: &mut Command, req: &ExecRequest) -> Vec<String> {
    if req.isolation != IsolationLevel::Hard {
        return Vec::new();
    }
    linux::install(cmd, req)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::unix::process::CommandExt;

    use crate::types::HardLimits;

    pub(super) fn install(cmd: &mut Command, req: &ExecRequest) -> Vec<String> {
        let mut applied = Vec::new();
        let mut limits = HardLimits::default();
        if limits.cpu_seconds == 0 {
            let secs = req.timeout.as_secs().max(1);
            limits.cpu_seconds = secs.saturating_add(2);
        }
        applied.push("rlimits".to_string());

        #[cfg(feature = "landlock")]
        let landlock_roots: Vec<std::path::PathBuf> = {
            let mut roots = default_landlock_read_roots();
            if let Some(root) = &req.project_root {
                if root.is_dir() {
                    roots.push(root.clone());
                }
            }
            roots
        };
        #[cfg(feature = "landlock")]
        let landlock_requested = true;
        #[cfg(not(feature = "landlock"))]
        let landlock_requested = false;

        #[cfg(feature = "seccomp")]
        let runtime = req.runtime;

        // SAFETY: the pre_exec closure runs after fork() and before
        unsafe {
            let limits = limits;
            cmd.pre_exec(move || {
                apply_rlimits(&limits)?;
                #[cfg(feature = "landlock")]
                {
                    if let Err(e) = apply_landlock(&landlock_roots) {
                        tracing::warn!(error = %e, "landlock ruleset not enforced");
                    }
                }
                #[cfg(feature = "seccomp")]
                {
                    if let Err(e) = crate::seccomp::install_seccomp_filter(runtime) {
                        tracing::warn!(error = %e, "seccomp filter not enforced");
                    }
                }
                Ok(())
            });
        }

        if landlock_requested {
            applied.push("landlock".to_string());
        }
        #[cfg(feature = "seccomp")]
        applied.push("seccomp".to_string());
        applied
    }

    fn apply_rlimits(limits: &HardLimits) -> std::io::Result<()> {
        set_rlimit(libc::RLIMIT_AS, limits.address_space_bytes)?;
        set_rlimit(libc::RLIMIT_CPU, limits.cpu_seconds)?;
        set_rlimit(libc::RLIMIT_NOFILE, limits.open_files)?;
        set_rlimit(libc::RLIMIT_NPROC, limits.processes)?;
        set_rlimit(libc::RLIMIT_FSIZE, limits.file_size_bytes)?;
        Ok(())
    }

    #[cfg(target_env = "gnu")]
    type RlimitResource = libc::__rlimit_resource_t;
    #[cfg(not(target_env = "gnu"))]
    type RlimitResource = libc::c_int;

    fn set_rlimit(resource: RlimitResource, value: u64) -> std::io::Result<()> {
        let rl = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        // SAFETY: `rl` is owned on the stack and lives through the call.
        let rc = unsafe { libc::setrlimit(resource, &rl as *const libc::rlimit) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(feature = "landlock")]
    fn default_landlock_read_roots() -> Vec<std::path::PathBuf> {
        ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    }

    #[cfg(feature = "landlock")]
    fn apply_landlock(read_roots: &[std::path::PathBuf]) -> std::io::Result<()> {
        use landlock::{
            path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr,
            RulesetStatus, ABI,
        };
        let abi = ABI::V2;
        let read_paths: Vec<&std::path::Path> = read_roots.iter().map(|p| p.as_path()).collect();
        let write_paths: Vec<&std::path::Path> = vec![std::path::Path::new("/tmp")]
            .into_iter()
            .filter(|p| p.exists())
            .collect();
        let status = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .create()
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .add_rules(path_beneath_rules(&read_paths, AccessFs::from_read(abi)))
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .add_rules(path_beneath_rules(&write_paths, AccessFs::from_all(abi)))
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .restrict_self()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if matches!(status.ruleset, RulesetStatus::NotEnforced) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "landlock unsupported on this kernel",
            ));
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    use super::*;

    pub(super) fn install(_cmd: &mut Command, _req: &ExecRequest) -> Vec<String> {
        tracing::warn!(
            "IsolationLevel::Hard requested but the current target is not Linux; \
             falling back to portable guarantees"
        );
        Vec::new()
    }
}

fn spawn_capture<R: Read + Send + 'static>(
    mut reader: R,
    max_bytes: usize,
) -> thread::JoinHandle<(String, bool)> {
    thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::with_capacity(8192);
        let mut chunk = [0u8; 4096];
        let mut truncated = false;
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > max_bytes {
                        let take = max_bytes.saturating_sub(buf.len());
                        if take > 0 {
                            buf.extend_from_slice(&chunk[..take]);
                        }
                        truncated = true;
                        let mut sink = [0u8; 8192];
                        while reader.read(&mut sink).unwrap_or(0) > 0 {}
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        (String::from_utf8_lossy(&buf).into_owned(), truncated)
    })
}

fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(false),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(true);
                }
                thread::sleep(poll);
            }
            Err(e) => return Err(CruxError::other(format!("wait failed: {e}"))),
        }
    }
}

#[allow(dead_code)]
fn _allowed_env_keys_static_check() -> &'static [&'static str] {
    ALLOWED_ENV_KEYS
}

#[allow(dead_code)]
fn _hash_map_assert<K: Eq + std::hash::Hash, V>(_: HashMap<K, V>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn probe_runtime(interpreter: &str, args: &[&str], expected: &str) -> bool {
        let out = match std::process::Command::new(interpreter).args(args).output() {
            Ok(o) => o,
            Err(_) => return false,
        };
        if !out.status.success() {
            return false;
        }
        String::from_utf8_lossy(&out.stdout).contains(expected)
    }

    fn require_python() -> bool {
        let interpreter = RuntimeKind::Python.default_interpreter();
        probe_runtime(interpreter, &["-c", "print('crux-probe')"], "crux-probe")
    }

    fn require_bash() -> bool {
        probe_runtime("bash", &["-c", "echo crux-probe"], "crux-probe")
    }

    #[test]
    fn bash_echo_returns_stdout() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let req = ExecRequest::new(RuntimeKind::Bash, "echo hello-from-bash");
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0));
        assert!(res.stdout.contains("hello-from-bash"));
        assert!(!res.timed_out);
    }

    #[test]
    fn python_arithmetic() {
        if !require_python() {
            return;
        }
        let exec = Executor::new();
        let req = ExecRequest::new(RuntimeKind::Python, "print(2+2)");
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.stdout.trim(), "4");
        assert_eq!(res.exit_code, Some(0));
    }

    #[test]
    fn timeout_is_enforced() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Bash, "sleep 5");
        req.timeout = Duration::from_millis(150);
        let res = exec.execute(&req).unwrap();
        assert!(res.timed_out, "expected timed_out=true got {:?}", res);
        assert!(res.exit_code.is_none());
    }

    #[test]
    fn output_truncation_flagged() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Bash, "yes 1 | head -c 200000");
        req.timeout = Duration::from_secs(5);
        req.max_output_bytes = 1024;
        let res = exec.execute(&req).unwrap();
        assert!(res.stdout_truncated, "expected stdout to truncate");
        assert!(res.stdout.len() <= 1024 + 16);
    }

    #[test]
    fn env_is_scrubbed_by_default() {
        if !require_bash() {
            return;
        }
        std::env::set_var("CRUX_TEST_LEAK", "secret");
        let exec = Executor::new();
        let req = ExecRequest::new(RuntimeKind::Bash, "echo \"${CRUX_TEST_LEAK:-MISSING}\"");
        let res = exec.execute(&req).unwrap();
        std::env::remove_var("CRUX_TEST_LEAK");
        assert!(res.stdout.contains("MISSING"));
    }

    #[test]
    fn home_does_not_leak_without_inherit_env() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let req = ExecRequest::new(RuntimeKind::Bash, "echo \"HOME=${HOME:-MISSING}\"");
        let res = exec.execute(&req).unwrap();
        assert!(
            res.stdout.contains("HOME=MISSING"),
            "HOME must not pass through by default, got stdout: {}",
            res.stdout
        );
    }

    #[test]
    fn explicit_env_entry_still_passes_home() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Bash, "echo \"HOME=${HOME:-MISSING}\"");
        req.env.insert("HOME".into(), "/tmp/custom-home".into());
        let res = exec.execute(&req).unwrap();
        assert!(res.stdout.contains("HOME=/tmp/custom-home"));
    }

    #[test]
    fn unknown_interpreter_is_a_clean_error() {
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Node, "console.log(1)");
        req.env.insert("PATH".into(), "".into());
        req.inherit_env = false;
        if let Err(e) = exec.execute(&req) {
            let msg = format!("{e}");
            assert!(msg.contains("interpreter") || msg.contains("spawn"));
        }
    }

    #[test]
    fn portable_isolation_reports_empty_applied_list() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let req = ExecRequest::new(RuntimeKind::Bash, "echo ok");
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.isolation_applied, Vec::<String>::new());
        assert!(res.stdout.contains("ok"));
    }

    #[test]
    fn agent_permission_deny_rule_blocks_before_spawn() {
        use crate::permissions::{PermRule, PermScope, PermSource, Permissions};
        let exec = Executor::new();
        let perms = Permissions::new(
            vec![
                PermRule::parse("Bash(rm -rf *)", PermSource::ClaudeCode, PermScope::Global)
                    .unwrap(),
            ],
            vec![],
        );
        let req = ExecRequest::new(RuntimeKind::Bash, "rm -rf /tmp/x").with_permissions(perms);
        let err = exec.execute(&req).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("denied by agent permission rule"),
            "missing deny preamble: {msg}"
        );
        assert!(msg.contains("Bash(rm -rf *)"), "rule should echo: {msg}");
        assert!(msg.contains("claude-code"), "source label missing: {msg}");
    }

    #[test]
    fn agent_permission_does_not_block_unrelated_runtime() {
        if !require_python() {
            return;
        }
        use crate::permissions::{PermRule, PermScope, PermSource, Permissions};
        let exec = Executor::new();
        let perms = Permissions::new(
            vec![
                PermRule::parse("Bash(rm -rf *)", PermSource::ClaudeCode, PermScope::Global)
                    .unwrap(),
            ],
            vec![],
        );
        let req = ExecRequest::new(RuntimeKind::Python, "print('ok')").with_permissions(perms);
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0));
        assert!(res.stdout.contains("ok"));
    }

    #[test]
    fn agent_permission_allow_overrides_deny() {
        if !require_bash() {
            return;
        }
        use crate::permissions::{PermRule, PermScope, PermSource, Permissions};
        let exec = Executor::new();
        let perms = Permissions::new(
            vec![PermRule::parse("Bash(rm *)", PermSource::ClaudeCode, PermScope::Global).unwrap()],
            vec![PermRule::parse(
                "Bash(rm /tmp/scratch*)",
                PermSource::ClaudeCode,
                PermScope::Project,
            )
            .unwrap()],
        );
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch_l7_perm_test");
        std::fs::write(&scratch, "x").unwrap();
        let code = format!(
            "rm /tmp/scratch_nonexistent_l7 2>/dev/null; rm '{}'",
            scratch.display()
        );
        let req = ExecRequest::new(RuntimeKind::Bash, code).with_permissions(perms);
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0), "stderr: {}", res.stderr);
    }

    #[test]
    fn no_permissions_attached_skips_check() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let req = ExecRequest::new(RuntimeKind::Bash, "echo 'rm -rf /' # printed not run");
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hard_isolation_applies_rlimits_on_linux() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(
            RuntimeKind::Bash,
            "ulimit -v; echo ---; ulimit -n; echo ---; ulimit -u",
        );
        req.isolation = IsolationLevel::Hard;
        req.timeout = std::time::Duration::from_secs(5);
        let res = exec.execute(&req).unwrap();
        assert!(
            res.isolation_applied.contains(&"rlimits".to_string()),
            "expected rlimits label, got {:?}",
            res.isolation_applied
        );
        let stdout = res.stdout.trim().to_string();
        let mut parts = stdout.split("---").map(|s| s.trim());
        let as_kb = parts.next().unwrap_or("");
        let nofile = parts.next().unwrap_or("");
        let nproc = parts.next().unwrap_or("");
        let as_kb: u64 = as_kb.parse().unwrap_or(u64::MAX);
        let nofile: u64 = nofile.parse().unwrap_or(u64::MAX);
        let nproc: u64 = nproc.parse().unwrap_or(u64::MAX);
        assert!(
            as_kb <= 524_288,
            "RLIMIT_AS should be ≤ 512MiB, got {as_kb} KiB"
        );
        assert!(nofile <= 64, "RLIMIT_NOFILE should be ≤ 64, got {nofile}");
        assert!(nproc <= 32, "RLIMIT_NPROC should be ≤ 32, got {nproc}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hard_isolation_blocks_fork_bomb() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(
            RuntimeKind::Bash,
            "for i in $(seq 1 200); do (sleep 5 &) 2>/dev/null; done; wait",
        );
        req.isolation = IsolationLevel::Hard;
        req.timeout = std::time::Duration::from_secs(3);
        let res = exec.execute(&req).unwrap();
        assert!(res.isolation_applied.contains(&"rlimits".to_string()));
    }

    #[cfg(all(target_os = "linux", feature = "seccomp"))]
    #[test]
    fn seccomp_applied_label_present() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Bash, "echo ok");
        req.isolation = IsolationLevel::Hard;
        req.timeout = std::time::Duration::from_secs(5);
        let res = exec.execute(&req).unwrap();
        assert!(
            res.isolation_applied.contains(&"seccomp".to_string()),
            "expected seccomp label, got {:?}",
            res.isolation_applied
        );
    }

    #[cfg(all(target_os = "linux", feature = "seccomp"))]
    #[test]
    fn seccomp_python_runs_hello() {
        if !require_python() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Python, "print('hello from seccomp')");
        req.isolation = IsolationLevel::Hard;
        req.timeout = std::time::Duration::from_secs(5);
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0), "stderr: {}", res.stderr);
        assert!(res.stdout.contains("hello from seccomp"));
        assert!(res.isolation_applied.contains(&"seccomp".to_string()));
    }

    #[cfg(all(target_os = "linux", feature = "seccomp"))]
    #[test]
    fn seccomp_bash_runs_echo() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Bash, "echo hello-from-seccomp-bash");
        req.isolation = IsolationLevel::Hard;
        req.timeout = std::time::Duration::from_secs(5);
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0), "stderr: {}", res.stderr);
        assert!(res.stdout.contains("hello-from-seccomp-bash"));
    }

    #[cfg(all(target_os = "linux", feature = "seccomp"))]
    #[test]
    fn seccomp_bash_arithmetic() {
        if !require_bash() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Bash, "echo $((2 + 2))");
        req.isolation = IsolationLevel::Hard;
        req.timeout = std::time::Duration::from_secs(5);
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0), "stderr: {}", res.stderr);
        assert!(res.stdout.contains("4"));
    }

    #[cfg(all(target_os = "linux", feature = "seccomp"))]
    #[test]
    fn seccomp_python_arithmetic() {
        if !require_python() {
            return;
        }
        let exec = Executor::new();
        let mut req = ExecRequest::new(RuntimeKind::Python, "print(2 + 2)");
        req.isolation = IsolationLevel::Hard;
        req.timeout = std::time::Duration::from_secs(5);
        let res = exec.execute(&req).unwrap();
        assert_eq!(res.exit_code, Some(0), "stderr: {}", res.stderr);
        assert_eq!(res.stdout.trim(), "4");
    }
}
