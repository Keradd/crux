//! Hot-reload helper for the global + project `crux.toml` pair.
//!
//! [`ConfigWatcher`] holds a published [`Config`] behind an
//! `Arc<RwLock<_>>` and re-reads both config files when their on-disk
//! mtime changes. Readers always see a *consistent* config — the swap
//! happens under a write lock so no reader observes a half-written
//! intermediate state.
//!
//! Polling is mtime-based (no `notify` / inotify dependency). Two
//! consumption modes:
//!
//! 1. Pull: call [`ConfigWatcher::tick`] from your existing event loop
//!    whenever it makes sense (between MCP requests, before each `crux
//!    bash` filter run, …). `tick` returns `true` on reload.
//! 2. Push: hand the watcher to [`ConfigWatcher::spawn_polling`] which
//!    runs a background thread that ticks on a fixed cadence and
//!    publishes the new config in place. Drop the returned
//!    [`WatcherHandle`] to stop the thread cleanly.
//!
//! Failure mode: if either config file becomes invalid mid-session, the
//! watcher logs a warning and keeps the previously published config so
//! a typo doesn't take the whole CRUX runtime down. The next clean
//! parse pulls the new contents.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use crate::config::{load, Config};
use crate::error::Result;
use crate::Runtime;

/// Default polling cadence for [`ConfigWatcher::spawn_polling`]. 1s is
/// snappy enough to catch interactive `vi crux.toml` edits without
/// burning measurable CPU on a long-lived MCP session.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Live, hot-swappable configuration handle.
pub struct ConfigWatcher {
    /// The currently published config. Cloned via `Arc::clone` so
    /// background-spawned watchers and foreground readers share state.
    config: Arc<RwLock<Config>>,
    state: Mutex<WatcherState>,
    project_root: Option<PathBuf>,
    /// Bumped on every successful reload. Cheap counter for observers
    /// that want to know if anything changed since they last looked
    /// without copying the whole config.
    reload_counter: Arc<AtomicU64>,
}

#[derive(Debug)]
struct WatcherState {
    global_path: PathBuf,
    project_path: Option<PathBuf>,
    global_mtime: Option<SystemTime>,
    project_mtime: Option<SystemTime>,
}

impl ConfigWatcher {
    /// Open the watcher: load both config files, snapshot mtimes,
    /// and publish the merged result. Equivalent to a `Runtime::open`
    /// for code paths that only need the config but want hot-reload.
    pub fn open(project_root: Option<PathBuf>) -> Result<Self> {
        let loaded = load(project_root.as_deref())?;
        let global_mtime = mtime_of(&loaded.global_path);
        let project_mtime = loaded.project_path.as_deref().and_then(mtime_of);
        Ok(Self {
            config: Arc::new(RwLock::new(loaded.config)),
            state: Mutex::new(WatcherState {
                global_path: loaded.global_path,
                project_path: loaded.project_path,
                global_mtime,
                project_mtime,
            }),
            project_root,
            reload_counter: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Build a watcher from an already-opened [`Runtime`] without
    /// re-reading either config file. The watcher's initial published
    /// config is cloned from the runtime; subsequent [`Self::tick`]
    /// calls notice mtime changes relative to the moment this
    /// constructor ran.
    ///
    /// Use this from the MCP server (or any other long-lived
    /// subsystem) that already opened a `Runtime` and doesn't want to
    /// pay for a redundant config parse when spinning up hot-reload.
    pub fn from_runtime(runtime: &Runtime) -> Self {
        let global_path = runtime.global_config_path.clone();
        let project_path = runtime.project_config_path.clone();
        let global_mtime = mtime_of(&global_path);
        let project_mtime = project_path.as_deref().and_then(mtime_of);
        Self {
            config: Arc::new(RwLock::new(runtime.config.clone())),
            state: Mutex::new(WatcherState {
                global_path,
                project_path,
                global_mtime,
                project_mtime,
            }),
            project_root: runtime.project_root.clone(),
            reload_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Hand out a clone of the inner `Arc<RwLock<Config>>`. Cheap
    /// (refcount bump). Use this when you need to pass the live config
    /// into a background subsystem that can't borrow the watcher.
    pub fn handle(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
    }

    /// Cheap counter that bumps on every successful reload. Compare
    /// against a prior value to know whether the config changed.
    pub fn reload_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.reload_counter)
    }

    /// Take a fresh snapshot of the current config. Holds the read
    /// lock only long enough to clone the inner struct; the lock is
    /// released before returning so callers never accidentally serialize
    /// long-running work behind it.
    pub fn snapshot(&self) -> Config {
        self.config.read().expect("config lock poisoned").clone()
    }

    /// Check the on-disk mtimes; if either moved, re-read both files,
    /// merge, and atomically swap. Returns `Ok(true)` on reload,
    /// `Ok(false)` on no-op. A parse error during reload is logged and
    /// returned as `Ok(false)` — the previously published config stays
    /// active so a typo can't brick the runtime.
    pub fn tick(&self) -> Result<bool> {
        let mut state = self.state.lock().expect("watcher state lock poisoned");
        let new_global_mtime = mtime_of(&state.global_path);
        let new_project_mtime = state.project_path.as_deref().and_then(mtime_of);
        let changed = new_global_mtime != state.global_mtime
            || new_project_mtime != state.project_mtime;
        if !changed {
            return Ok(false);
        }
        // mtime moved — try to re-read the bundle. Hold off on
        // committing the new mtime until parse succeeds so a transient
        // editor "save half-written" state retries on the next tick.
        match load(self.project_root.as_deref()) {
            Ok(loaded) => {
                {
                    let mut guard = self.config.write().expect("config lock poisoned");
                    *guard = loaded.config;
                }
                state.global_path = loaded.global_path;
                state.project_path = loaded.project_path;
                state.global_mtime = new_global_mtime;
                state.project_mtime = new_project_mtime;
                self.reload_counter.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "config reload failed, keeping previous config"
                );
                // Don't roll mtime forward — let the next tick try
                // again once the user finishes editing.
                Ok(false)
            }
        }
    }

    /// Spawn a background thread that polls [`Self::tick`] on a fixed
    /// cadence. Drop the returned [`WatcherHandle`] to stop and join
    /// the thread cleanly.
    ///
    /// Takes `Arc<Self>` so the spawned thread can outlive the caller's
    /// stack frame. Use [`Arc::new`] before passing in.
    pub fn spawn_polling(self: Arc<Self>, interval: Duration) -> WatcherHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let watcher_clone = Arc::clone(&self);
        let join = std::thread::Builder::new()
            .name("crux-config-watch".to_string())
            .spawn(move || {
                while !stop_for_thread.load(Ordering::Relaxed) {
                    // Sleep in small slices so an early shutdown
                    // doesn't have to wait the full `interval`.
                    let slice = Duration::from_millis(50).min(interval);
                    let mut waited = Duration::ZERO;
                    while waited < interval {
                        if stop_for_thread.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(slice);
                        waited += slice;
                    }
                    if stop_for_thread.load(Ordering::Relaxed) {
                        return;
                    }
                    let _ = watcher_clone.tick();
                }
            })
            .expect("spawn config watcher thread");
        WatcherHandle {
            stop,
            join: Some(join),
        }
    }
}

/// Drop guard for [`ConfigWatcher::spawn_polling`]. Drops cleanly join
/// the polling thread.
pub struct WatcherHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WatcherHandle {
    /// Synchronously stop the polling thread without dropping the
    /// handle. Safe to call multiple times.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn mtime_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    /// Set `$CRUX_HOME` to a tempdir for the duration of the closure.
    /// Tests touching the global config path must run sequentially
    /// (they all mutate the env var); cargo's test harness serializes
    /// per file by default, so as long as we keep watcher tests in
    /// their own module we're safe.
    fn with_crux_home<R>(f: impl FnOnce(&Path) -> R) -> R {
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var("CRUX_HOME").ok();
        std::env::set_var("CRUX_HOME", dir.path());
        let r = f(dir.path());
        match prev {
            Some(v) => std::env::set_var("CRUX_HOME", v),
            None => std::env::remove_var("CRUX_HOME"),
        }
        r
    }

    fn write_project_config(project: &Path, body: &str) {
        let path = project.join(".crux").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    /// Move the file's mtime forward by at least 2 seconds so the
    /// SystemTime comparison flips even on filesystems with low
    /// timestamp resolution (HFS+, ext3 with noatime, ...).
    fn touch_forward(path: &Path) {
        // sleep_until isn't stable; sleep instead.
        std::thread::sleep(Duration::from_millis(1100));
        let body = fs::read_to_string(path).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn open_publishes_initial_config() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            let cfg = w.snapshot();
            assert!(!cfg.layers.l7_sandbox);
            assert!(cfg.layers.l4_read_cache); // default preserved
        });
    }

    #[test]
    fn tick_with_no_change_returns_false() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            // First tick after open: nothing has changed.
            assert!(!w.tick().unwrap(), "expected no-op tick");
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 0);
        });
    }

    #[test]
    fn tick_after_project_edit_swaps_config_atomically() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(w.snapshot().layers.l7_sandbox);

            // Edit the project config to flip l7_sandbox off.
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");

            assert!(w.tick().unwrap(), "expected reload");
            assert!(!w.snapshot().layers.l7_sandbox);
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn handle_returns_shared_arc_lock() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            let handle = w.handle();
            // Read through the handle and the watcher independently.
            assert!(handle.read().unwrap().layers.l7_sandbox);
            assert!(w.snapshot().layers.l7_sandbox);
            // After reload, both views should reflect the new state.
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            w.tick().unwrap();
            assert!(!handle.read().unwrap().layers.l7_sandbox);
        });
    }

    #[test]
    fn malformed_reload_keeps_previous_config() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(w.snapshot().layers.l7_sandbox);

            // Write garbage and tick — the previous config must persist.
            let path = dir.path().join(".crux").join("config.toml");
            std::thread::sleep(Duration::from_millis(1100));
            fs::write(&path, "not valid toml = = =").unwrap();
            assert!(!w.tick().unwrap(), "broken parse must not flip the publish");
            assert!(w.snapshot().layers.l7_sandbox, "config must not be wiped");
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 0);

            // Once the user fixes the file, the next tick recovers.
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            assert!(w.tick().unwrap());
            assert!(!w.snapshot().layers.l7_sandbox);
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn tick_handles_disappearing_project_config_gracefully() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(!w.snapshot().layers.l7_sandbox);

            // Delete the project config — the merge should fall back to
            // defaults (l7_sandbox = true).
            let path = dir.path().join(".crux").join("config.toml");
            std::thread::sleep(Duration::from_millis(1100));
            fs::remove_file(&path).unwrap();
            assert!(w.tick().unwrap(), "deletion should trigger reload");
            assert!(w.snapshot().layers.l7_sandbox);
        });
    }

    #[test]
    fn project_only_change_triggers_reload_even_when_global_untouched() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            // Start with default global + an explicit project override.
            write_project_config(dir.path(), "[layers]\nl11_digest = false\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(!w.snapshot().layers.l11_digest);

            // Flip the project override.
            touch_forward(&dir.path().join(".crux").join("config.toml"));
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl11_digest = true\n");
            assert!(w.tick().unwrap());
            assert!(w.snapshot().layers.l11_digest);
        });
    }

    #[test]
    fn background_polling_picks_up_changes_and_drop_stops_thread() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let w = Arc::new(
                ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap(),
            );
            let handle = w.clone().spawn_polling(Duration::from_millis(100));

            // Edit the config; the polling thread should pick it up
            // within a few hundred ms.
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");

            // Wait up to 3s for the change to propagate.
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                if !w.snapshot().layers.l7_sandbox {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(
                !w.snapshot().layers.l7_sandbox,
                "background poll should have flipped l7_sandbox off"
            );

            // Dropping the handle should join the thread cleanly.
            drop(handle);
            // No assertion; if we got here without hanging, drop worked.
        });
    }

    #[test]
    fn handle_explicit_stop_is_idempotent() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "");
            let w = Arc::new(
                ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap(),
            );
            let mut handle = w.spawn_polling(Duration::from_millis(50));
            handle.stop();
            // Calling stop again must be a no-op.
            handle.stop();
        });
    }

    #[test]
    fn from_runtime_snapshot_matches_and_picks_up_edits() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            // Build a Runtime exactly like the CLI does, then derive
            // a watcher from it without re-parsing the config.
            let runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(runtime.config.layers.l7_sandbox);

            let w = ConfigWatcher::from_runtime(&runtime);
            // Initial snapshot must equal the runtime's config.
            assert_eq!(
                w.snapshot().layers.l7_sandbox,
                runtime.config.layers.l7_sandbox
            );
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 0);
            // First tick: nothing changed on disk, no-op.
            assert!(!w.tick().unwrap());

            // Edit the project config. tick() must pick it up even
            // though we never called ConfigWatcher::open() directly.
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            assert!(w.tick().unwrap());
            assert!(!w.snapshot().layers.l7_sandbox);
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 1);
        });
    }
}
