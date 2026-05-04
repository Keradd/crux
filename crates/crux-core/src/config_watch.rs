use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::config::{load, Config};
use crate::error::Result;
use crate::Runtime;

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub struct ConfigWatcher {
    config: Arc<RwLock<Config>>,
    state: Mutex<WatcherState>,
    project_root: Option<PathBuf>,
    reload_counter: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileStamp {
    mtime: Option<SystemTime>,
    size: Option<u64>,
    hash: Option<[u8; 32]>,
}

fn stamp_of(path: &Path) -> FileStamp {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return FileStamp::default(),
    };
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map(|m| m.len());
    let mtime = meta.and_then(|m| m.modified().ok());
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash: [u8; 32] = hasher.finalize().into();
    FileStamp {
        mtime,
        size,
        hash: Some(hash),
    }
}

#[derive(Debug)]
struct WatcherState {
    global_path: PathBuf,
    project_path: Option<PathBuf>,
    global_stamp: FileStamp,
    project_stamp: FileStamp,
}

impl ConfigWatcher {
    pub fn open(project_root: Option<PathBuf>) -> Result<Self> {
        let loaded = load(project_root.as_deref())?;
        let global_stamp = stamp_of(&loaded.global_path);
        let project_stamp = loaded
            .project_path
            .as_deref()
            .map(stamp_of)
            .unwrap_or_default();
        Ok(Self {
            config: Arc::new(RwLock::new(loaded.config)),
            state: Mutex::new(WatcherState {
                global_path: loaded.global_path,
                project_path: loaded.project_path,
                global_stamp,
                project_stamp,
            }),
            project_root,
            reload_counter: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn from_runtime(runtime: &Runtime) -> Self {
        let global_path = runtime.global_config_path.clone();
        let project_path = runtime.project_config_path.clone();
        let global_stamp = stamp_of(&global_path);
        let project_stamp = project_path
            .as_deref()
            .map(stamp_of)
            .unwrap_or_default();
        Self {
            config: Arc::new(RwLock::new(runtime.config.clone())),
            state: Mutex::new(WatcherState {
                global_path,
                project_path,
                global_stamp,
                project_stamp,
            }),
            project_root: runtime.project_root.clone(),
            reload_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn handle(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
    }

    pub fn reload_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.reload_counter)
    }

    pub fn snapshot(&self) -> Config {
        self.config.read().expect("config lock poisoned").clone()
    }

    pub fn tick(&self) -> Result<bool> {
        let mut state = self.state.lock().expect("watcher state lock poisoned");
        let new_global_stamp = stamp_of(&state.global_path);
        let new_project_stamp = state
            .project_path
            .as_deref()
            .map(stamp_of)
            .unwrap_or_default();
        let changed = new_global_stamp != state.global_stamp
            || new_project_stamp != state.project_stamp;
        if !changed {
            return Ok(false);
        }
        match load(self.project_root.as_deref()) {
            Ok(loaded) => {
                {
                    let mut guard = self.config.write().expect("config lock poisoned");
                    *guard = loaded.config;
                }
                state.global_path = loaded.global_path;
                state.project_path = loaded.project_path;
                state.global_stamp = new_global_stamp;
                state.project_stamp = new_project_stamp;
                self.reload_counter.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "config reload failed, keeping previous config"
                );
                Ok(false)
            }
        }
    }

    pub fn spawn_polling(self: Arc<Self>, interval: Duration) -> WatcherHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let watcher_clone = Arc::clone(&self);
        let join = std::thread::Builder::new()
            .name("crux-config-watch".to_string())
            .spawn(move || {
                while !stop_for_thread.load(Ordering::Relaxed) {
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

pub struct WatcherHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WatcherHandle {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use std::time::Duration;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_crux_home<R>(f: impl FnOnce(&Path) -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    fn touch_forward(path: &Path) {
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
            assert!(handle.read().unwrap().layers.l7_sandbox);
            assert!(w.snapshot().layers.l7_sandbox);
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

            let path = dir.path().join(".crux").join("config.toml");
            std::thread::sleep(Duration::from_millis(1100));
            fs::write(&path, "not valid toml = = =").unwrap();
            assert!(!w.tick().unwrap(), "broken parse must not flip the publish");
            assert!(w.snapshot().layers.l7_sandbox, "config must not be wiped");
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 0);

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
            write_project_config(dir.path(), "[layers]\nl11_digest = false\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(!w.snapshot().layers.l11_digest);

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
            let w = Arc::new(ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap());
            let handle = w.clone().spawn_polling(Duration::from_millis(100));

            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");

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

            drop(handle);
        });
    }

    #[test]
    fn handle_explicit_stop_is_idempotent() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "");
            let w = Arc::new(ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap());
            let mut handle = w.spawn_polling(Duration::from_millis(50));
            handle.stop();
            handle.stop();
        });
    }

    #[test]
    fn from_runtime_snapshot_matches_and_picks_up_edits() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(runtime.config.layers.l7_sandbox);

            let w = ConfigWatcher::from_runtime(&runtime);
            assert_eq!(
                w.snapshot().layers.l7_sandbox,
                runtime.config.layers.l7_sandbox
            );
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 0);
            assert!(!w.tick().unwrap());

            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            assert!(w.tick().unwrap());
            assert!(!w.snapshot().layers.l7_sandbox);
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn tick_detects_rapid_content_change_without_mtime_wait() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(".crux").join("config.toml");
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let w = ConfigWatcher::open(Some(dir.path().to_path_buf())).unwrap();
            assert!(w.snapshot().layers.l7_sandbox);

            fs::write(&path, "[layers]\nl7_sandbox = false\n").unwrap();
            assert!(
                w.tick().unwrap(),
                "size+hash stamp must catch rapid within-second edits"
            );
            assert!(!w.snapshot().layers.l7_sandbox);
            assert_eq!(w.reload_counter().load(Ordering::Relaxed), 1);
        });
    }
}
