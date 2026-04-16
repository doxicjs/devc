//! Polls `devc.toml` (and optional `devc.local.toml`) by mtime, debounces
//! bursty writes (~100ms window), and reports a `WatchEvent` per poll.
//!
//! Tolerates up to 3 consecutive failed reads (atomic-rename window) before
//! emitting a `Notice` so editors that rename-on-save don't flash errors.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::Config;

const DEBOUNCE: Duration = Duration::from_millis(100);
const MISSING_GRACE_TICKS: u8 = 3;

pub enum WatchEvent {
    Idle,
    Reloaded(Config),
    Error(String),
    Notice(String),
}

pub struct ConfigWatcher {
    main_path: PathBuf,
    local_path: Option<PathBuf>,
    main_mtime: Option<SystemTime>,
    local_mtime: Option<SystemTime>,
    reload_pending_since: Option<Instant>,
    reload_fail_count: u8,
}

impl ConfigWatcher {
    pub fn new(main_path: PathBuf, local_path: Option<PathBuf>) -> Self {
        let main_mtime = file_mtime(&main_path).ok().flatten();
        let local_mtime = local_path.as_ref().and_then(|p| file_mtime(p).ok().flatten());
        Self {
            main_path,
            local_path,
            main_mtime,
            local_mtime,
            reload_pending_since: None,
            reload_fail_count: 0,
        }
    }

    pub fn poll(&mut self) -> WatchEvent {
        let main_mtime = match file_mtime(&self.main_path) {
            Ok(Some(m)) => {
                self.reload_fail_count = 0;
                Some(m)
            }
            Ok(None) | Err(_) => {
                self.reload_fail_count = self.reload_fail_count.saturating_add(1);
                if self.reload_fail_count == MISSING_GRACE_TICKS {
                    return WatchEvent::Notice("config file missing".to_string());
                }
                return WatchEvent::Idle;
            }
        };

        let local_mtime = match self.local_path.as_ref() {
            Some(p) => file_mtime(p).ok().flatten(),
            None => None,
        };

        let main_changed = main_mtime != self.main_mtime;
        let local_changed = local_mtime != self.local_mtime;
        if !main_changed && !local_changed && self.reload_pending_since.is_none() {
            return WatchEvent::Idle;
        }

        if self.reload_pending_since.is_none() {
            self.reload_pending_since = Some(Instant::now());
            return WatchEvent::Idle;
        }
        if self.reload_pending_since.unwrap().elapsed() < DEBOUNCE {
            return WatchEvent::Idle;
        }

        match Config::load(&self.main_path, self.local_path.as_deref()) {
            Ok(new_cfg) => {
                self.main_mtime = main_mtime;
                self.local_mtime = local_mtime;
                self.reload_pending_since = None;
                WatchEvent::Reloaded(new_cfg)
            }
            Err(e) => {
                self.reload_pending_since = None;
                // Leave stored mtimes untouched so a subsequent edit re-triggers.
                WatchEvent::Error(format!("config reload failed: {}", e))
            }
        }
    }
}

fn file_mtime(path: &Path) -> Result<Option<SystemTime>, std::io::Error> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(m.modified().ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("devc-watcher-test-{}", std::process::id()))
            .join(format!("{}", rand_u64()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn rand_u64() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn write_toml(path: &Path, contents: &str) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn poll_idle_when_nothing_changes() {
        let dir = tempdir();
        let main = dir.join("devc.toml");
        write_toml(&main, "");
        let mut w = ConfigWatcher::new(main, None);
        assert!(matches!(w.poll(), WatchEvent::Idle));
        assert!(matches!(w.poll(), WatchEvent::Idle));
    }

    #[test]
    fn missing_file_emits_notice_after_grace() {
        let dir = tempdir();
        let missing = dir.join("nope.toml");
        let mut w = ConfigWatcher::new(missing, None);
        assert!(matches!(w.poll(), WatchEvent::Idle));
        assert!(matches!(w.poll(), WatchEvent::Idle));
        assert!(matches!(w.poll(), WatchEvent::Notice(_)));
    }
}
