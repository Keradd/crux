use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Python,
    Bash,
    Node,
}

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

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub runtime: RuntimeKind,
    pub code: String,
    pub project_root: Option<PathBuf>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub env: HashMap<String, String>,
    pub inherit_env: bool,
    pub isolation: IsolationLevel,
    pub permissions: Option<crate::permissions::Permissions>,
}

#[derive(Debug, Clone, Copy)]
pub struct HardLimits {
    pub address_space_bytes: u64,
    pub cpu_seconds: u64,
    pub open_files: u64,
    pub processes: u64,
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
            permissions: None,
        }
    }

    pub fn with_permissions(mut self, perms: crate::permissions::Permissions) -> Self {
        self.permissions = Some(perms);
        self
    }
}

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
    #[serde(default)]
    pub isolation_applied: Vec<String>,
}
