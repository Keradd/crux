//! Shared types for the Layer 7 sandbox executor.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Which interpreter to spawn for a given snippet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Python,
    Bash,
    Node,
}

/// How aggressively to isolate the child process.
///
/// - `Portable` (default) — time + output volume + env scrubbing + cwd
///   anchoring only. Works on every OS the Rust toolchain supports.
/// - `Hard` — layer additional kernel-level restrictions on Linux:
///   `setrlimit` caps for address space, CPU time, open files and
///   forks, plus landlock filesystem confinement when the crate is
///   compiled with the `landlock` feature. On non-Linux systems
///   `Hard` falls back to portable guarantees and emits a tracing
///   warning so callers can detect the downgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IsolationLevel {
    #[default]
    Portable,
    Hard,
}

impl IsolationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            IsolationLevel::Portable => "portable",
            IsolationLevel::Hard => "hard",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "portable" | "soft" | "default" => Self::Portable,
            "hard" | "strict" | "locked" => Self::Hard,
            _ => return None,
        })
    }
}

impl RuntimeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeKind::Python => "python",
            RuntimeKind::Bash => "bash",
            RuntimeKind::Node => "node",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "python" | "py" | "python3" => Self::Python,
            "bash" | "sh" | "shell" => Self::Bash,
            "node" | "js" | "javascript" | "deno" => Self::Node,
            _ => return None,
        })
    }

    /// Default executable name used when no path is provided. Each can
    /// be overridden via `[layer.l7.runtimes.<kind>] interpreter = "..."`.
    ///
    /// Windows note: the official python.org installer ships `python.exe`
    /// but no `python3.exe`, so calls to `python3` get caught by the
    /// Microsoft Store launcher stub at `WindowsApps\python3.exe`, which
    /// exits 0 with empty stdout instead of running anything. Pick the
    /// platform-native name to dodge the stub.
    pub fn default_interpreter(self) -> &'static str {
        match self {
            RuntimeKind::Python => {
                #[cfg(target_os = "windows")]
                {
                    "python"
                }
                #[cfg(not(target_os = "windows"))]
                {
                    "python3"
                }
            }
            RuntimeKind::Bash => "bash",
            RuntimeKind::Node => "node",
        }
    }
}

/// Caller-supplied execution request.
#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub runtime: RuntimeKind,
    pub code: String,
    pub project_root: Option<PathBuf>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    /// Extra env vars merged on top of the scrubbed default set.
    pub env: HashMap<String, String>,
    /// If `false` (default) the child runs with a tightly scrubbed `PATH`
    /// and most parent env stripped. Toggle to inherit.
    pub inherit_env: bool,
    /// How hard to isolate the child — see [`IsolationLevel`] for the
    /// matrix of guarantees per target.
    pub isolation: IsolationLevel,
}

/// Caps applied to the child when running under [`IsolationLevel::Hard`]
/// on Linux. All values are "soft" rlimits so the child can still fail
/// gracefully; exceeding them yields `SIGKILL`/`ENOMEM`/`EMFILE`/etc.
#[derive(Debug, Clone, Copy)]
pub struct HardLimits {
    /// Max address space in bytes (`RLIMIT_AS`). Default 512 MiB.
    pub address_space_bytes: u64,
    /// Max CPU seconds (`RLIMIT_CPU`). Computed from the wall-clock
    /// timeout in `Executor::execute` when left at `0`.
    pub cpu_seconds: u64,
    /// Max number of open file descriptors (`RLIMIT_NOFILE`).
    pub open_files: u64,
    /// Max number of processes / threads the child can spawn
    /// (`RLIMIT_NPROC`).
    pub processes: u64,
    /// Largest single file the child is allowed to create
    /// (`RLIMIT_FSIZE`). Default 64 MiB.
    pub file_size_bytes: u64,
}

impl Default for HardLimits {
    fn default() -> Self {
        Self {
            address_space_bytes: 512 * 1024 * 1024,
            cpu_seconds: 0, // derived from `timeout` when 0
            open_files: 64,
            processes: 32,
            file_size_bytes: 64 * 1024 * 1024,
        }
    }
}

impl ExecRequest {
    pub fn new(runtime: RuntimeKind, code: impl Into<String>) -> Self {
        Self {
            runtime,
            code: code.into(),
            project_root: None,
            timeout: Duration::from_secs(10),
            max_output_bytes: 64 * 1024,
            env: HashMap::new(),
            inherit_env: false,
            isolation: IsolationLevel::default(),
        }
    }
}

/// Result of a sandboxed run. `stdout` / `stderr` may be truncated; check
/// the `_truncated` flags before relying on the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub runtime: RuntimeKind,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub elapsed_ms: u128,
    /// Which isolation primitives were actually engaged for this run.
    /// Populated by the executor from the running target + compile-time
    /// feature flags. Callers can use this to verify that `Hard` didn't
    /// silently downgrade to `Portable`.
    #[serde(default)]
    pub isolation_applied: Vec<String>,
}
