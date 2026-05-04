//! CRUX Layer 7 — sandbox executor.
//!
//! Goal: let the agent "think in code" — write a snippet, run it
//! in an isolated subprocess, return only the result. Saves tokens by
//! replacing whole "explain step-by-step" exchanges with a single
//! deterministic computation.
//!
//! Public surface:
//! - [`Executor`] — one-shot subprocess runner.
//! - [`ExecRequest`] / [`ExecResult`] / [`RuntimeKind`].
//!
//! Security boundary:
//!
//! - [`IsolationLevel::Portable`] (default) — time + output volume + env
//!   scrubbing + cwd anchoring. Works on every supported OS.
//! - [`IsolationLevel::Hard`] — adds `setrlimit` caps (`RLIMIT_AS`,
//!   `RLIMIT_CPU`, `RLIMIT_NOFILE`, `RLIMIT_NPROC`, `RLIMIT_FSIZE`) on
//!   Linux, plus landlock filesystem confinement when the crate is
//!   compiled with the `landlock` cargo feature and the host kernel
//!   supports it. Seccomp syscall filtering is still tracked as future
//!   work. On non-Linux targets `Hard` degrades to `Portable` and the
//!   `isolation_applied` vector on [`ExecResult`] reflects what was
//!   actually engaged.

pub mod agent_perms;
pub mod executor;
pub mod permissions;
#[cfg(all(target_os = "linux", feature = "seccomp"))]
pub mod seccomp;
pub mod types;

pub use executor::Executor;
pub use permissions::{PermDecision, PermRule, PermScope, PermSource, Permissions};
pub use types::{ExecRequest, ExecResult, HardLimits, IsolationLevel, RuntimeKind};
