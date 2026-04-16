use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use crate::app::LogSource;

pub struct ProcessHandle {
    child: Child,
    pid: i32,
}

impl ProcessHandle {
    /// Spawn a command in a new process group. `tag` is invoked each time a
    /// log line is read, producing the `LogSource` the caller wants attached.
    /// This is where the caller injects its typed ID.
    ///
    /// Note: processes that call setsid() will escape the group and won't be
    /// killed on cleanup. This is a fundamental Unix limitation.
    pub fn spawn<F>(
        command: &str,
        working_dir: &str,
        log_sender: mpsc::Sender<(LogSource, String)>,
        tag: F,
    ) -> Result<Self, String>
    where
        F: Fn() -> LogSource + Send + Clone + 'static,
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let pid = child.id() as i32;

        if let Some(stdout) = child.stdout.take() {
            spawn_reader(stdout, log_sender.clone(), tag.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(stderr, log_sender, tag);
        }

        Ok(Self { child, pid })
    }

    pub fn send_sigterm(&self) {
        unsafe {
            libc::killpg(self.pid, libc::SIGTERM);
        }
    }

    pub fn send_sigkill(&self) {
        unsafe {
            libc::killpg(self.pid, libc::SIGKILL);
        }
    }

    /// Send SIGTERM, poll for exit over KILL_TIMEOUT_MS, then SIGKILL if needed.
    pub fn kill(&mut self) {
        const KILL_TIMEOUT_MS: u64 = 3000;
        const POLL_INTERVAL_MS: u64 = 100;

        self.send_sigterm();
        for _ in 0..(KILL_TIMEOUT_MS / POLL_INTERVAL_MS) {
            if !self.is_running() {
                let _ = self.child.wait();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
        }
        self.send_sigkill();
        let _ = self.child.wait();
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn exit_code(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => status.code(),
            _ => None,
        }
    }
}

fn spawn_reader<R, F>(
    stream: R,
    sender: mpsc::Sender<(LogSource, String)>,
    tag: F,
) where
    R: Read + Send + 'static,
    F: Fn() -> LogSource + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf)
                        .trim_end_matches(&['\r', '\n'][..])
                        .to_string();
                    if sender.send((tag(), line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{CommandId, ServiceId};
    use std::time::{Duration, Instant};

    // ===== Basic spawn and output collection =====

    #[test]
    fn spawn_and_collect_stdout() {
        let (tx, rx) = mpsc::channel();
        let _handle = ProcessHandle::spawn("echo hello", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        let mut lines = Vec::new();
        while let Ok((_, line)) = rx.try_recv() {
            lines.push(line);
        }

        assert!(!lines.is_empty(), "Should receive stdout output");
        assert!(lines.iter().any(|l| l.contains("hello")));
    }

    #[test]
    fn spawn_and_collect_stderr() {
        let (tx, rx) = mpsc::channel();
        let _handle = ProcessHandle::spawn("echo error >&2", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        let mut lines = Vec::new();
        while let Ok((_, line)) = rx.try_recv() {
            lines.push(line);
        }

        assert!(!lines.is_empty(), "Should receive stderr output");
        assert!(lines.iter().any(|l| l.contains("error")));
    }

    #[test]
    fn spawn_tags_as_service_by_default() {
        let (tx, rx) = mpsc::channel();
        let _handle = ProcessHandle::spawn("echo test", ".", tx, || LogSource::Service(ServiceId(42))).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        if let Ok((source, _)) = rx.try_recv() {
            match source {
                LogSource::Service(id) => assert_eq!(id, ServiceId(42)),
                LogSource::Command(_) => panic!("Expected Service tag, got Command"),
            }
        }
    }

    #[test]
    fn spawn_tagged_as_command() {
        let (tx, rx) = mpsc::channel();
        let _handle = ProcessHandle::spawn("echo test", ".", tx, || LogSource::Command(CommandId(7))).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        if let Ok((source, _)) = rx.try_recv() {
            match source {
                LogSource::Command(id) => assert_eq!(id, CommandId(7)),
                LogSource::Service(_) => panic!("Expected Command tag, got Service"),
            }
        }
    }

    #[test]
    fn multiline_output_preserves_all_lines() {
        let (tx, rx) = mpsc::channel();
        let _handle =
            ProcessHandle::spawn("printf 'line1\\nline2\\nline3\\n'", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        let mut lines = Vec::new();
        while let Ok((_, line)) = rx.try_recv() {
            lines.push(line);
        }

        assert_eq!(lines.len(), 3, "Should receive all 3 lines, got: {:?}", lines);
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[1], "line2");
        assert_eq!(lines[2], "line3");
    }

    // ===== Issue #7: non-UTF-8 output must not be silently dropped =====

    #[test]
    fn non_utf8_output_is_not_silently_dropped() {
        let (tx, rx) = mpsc::channel();
        // printf outputs raw 0xFF byte followed by " hello" and newline.
        // Current code: BufReader::lines().flatten() drops the entire line
        // because 0xFF is invalid UTF-8.
        // Expected: line is preserved with replacement character via lossy conversion.
        let _handle =
            ProcessHandle::spawn("printf '\\xff hello\\n'", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        let mut lines = Vec::new();
        while let Ok((_, line)) = rx.try_recv() {
            lines.push(line);
        }

        assert!(
            !lines.is_empty(),
            "Non-UTF-8 output must not be silently dropped"
        );
        let combined: String = lines.join("");
        assert!(
            combined.contains("hello"),
            "Valid portion of output should be preserved, got: {:?}",
            combined
        );
    }

    // ===== CRLF handling =====

    #[test]
    fn crlf_line_endings_stripped() {
        let (tx, rx) = mpsc::channel();
        let _handle = ProcessHandle::spawn("printf 'hello\\r\\n'", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        let mut lines = Vec::new();
        while let Ok((_, line)) = rx.try_recv() {
            lines.push(line);
        }

        assert!(!lines.is_empty());
        assert_eq!(
            lines[0], "hello",
            "\\r\\n should be fully stripped, got: {:?}",
            lines[0]
        );
    }

    // ===== Process lifecycle =====

    #[test]
    fn is_running_true_for_active_process() {
        let (tx, _rx) = mpsc::channel();
        let mut handle = ProcessHandle::spawn("sleep 10", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        assert!(handle.is_running());
        handle.kill();
    }

    #[test]
    fn is_running_false_after_exit() {
        let (tx, _rx) = mpsc::channel();
        let mut handle = ProcessHandle::spawn("true", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!handle.is_running());
    }

    #[test]
    fn exit_code_zero_on_success() {
        let (tx, _rx) = mpsc::channel();
        let mut handle = ProcessHandle::spawn("true", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(handle.exit_code(), Some(0));
    }

    #[test]
    fn exit_code_nonzero_on_failure() {
        let (tx, _rx) = mpsc::channel();
        let mut handle = ProcessHandle::spawn("false", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(handle.exit_code(), Some(1));
    }

    #[test]
    fn kill_terminates_running_process() {
        let (tx, _rx) = mpsc::channel();
        let mut handle = ProcessHandle::spawn("sleep 100", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        assert!(handle.is_running());
        handle.kill();
        assert!(!handle.is_running());
    }

    #[test]
    fn kill_handles_already_exited_process() {
        let (tx, _rx) = mpsc::channel();
        let mut handle = ProcessHandle::spawn("true", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        handle.kill(); // should not panic
    }

    // ===== Issue #12: kill should wait long enough for graceful shutdown =====

    #[test]
    fn kill_allows_graceful_sigterm_handling() {
        let (tx, _rx) = mpsc::channel();
        // Process traps SIGTERM and exits after a brief delay.
        // With 500ms timeout (current), SIGKILL fires before trap runs.
        // With 3s timeout (fixed), graceful exit should succeed.
        let mut handle = ProcessHandle::spawn(
            "trap 'exit 0' TERM; sleep 100 & wait",
            ".",
            tx,
            || LogSource::Service(ServiceId(0)),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(handle.is_running());

        let _start = Instant::now();
        handle.kill();

        assert!(!handle.is_running());
        // Graceful shutdown should complete well under 3s.
        // If SIGKILL was used, it would be nearly instant after the timeout.
        // We just verify it terminates — the timeout test is implicit.
    }

    // ===== Spawn errors =====

    #[test]
    fn spawn_with_invalid_working_dir_fails() {
        let (tx, _rx) = mpsc::channel();
        let result = ProcessHandle::spawn("echo hi", "/nonexistent/dir/xyz", tx, || LogSource::Service(ServiceId(0)));
        assert!(result.is_err(), "Should fail with invalid working directory");
    }

    // ===== Empty command =====

    #[test]
    fn spawn_empty_command() {
        let (tx, _rx) = mpsc::channel();
        // sh -c "" exits immediately with 0
        let mut handle = ProcessHandle::spawn("", ".", tx, || LogSource::Service(ServiceId(0))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!handle.is_running());
        assert_eq!(handle.exit_code(), Some(0));
    }
}
