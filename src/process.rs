use std::io::{BufRead, BufReader};
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
    pub fn spawn(
        command: &str,
        working_dir: &str,
        log_sender: mpsc::Sender<(LogSource, String)>,
        service_idx: usize,
    ) -> Result<Self, String> {
        Self::spawn_tagged(command, working_dir, log_sender, service_idx, false)
    }

    pub fn spawn_tagged(
        command: &str,
        working_dir: &str,
        log_sender: mpsc::Sender<(LogSource, String)>,
        idx: usize,
        is_command: bool,
    ) -> Result<Self, String> {
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

        let make_source = move |i: usize| -> LogSource {
            if is_command {
                LogSource::Command(i)
            } else {
                LogSource::Service(i)
            }
        };

        if let Some(stdout) = child.stdout.take() {
            let sender = log_sender.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().flatten() {
                    if sender.send((make_source(idx), line)).is_err() {
                        break;
                    }
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let sender = log_sender;
            let make_source2 = move |i: usize| -> LogSource {
                if is_command {
                    LogSource::Command(i)
                } else {
                    LogSource::Service(i)
                }
            };
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    if sender.send((make_source2(idx), line)).is_err() {
                        break;
                    }
                }
            });
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

    pub fn kill(&mut self) {
        self.send_sigterm();
        std::thread::sleep(std::time::Duration::from_millis(500));
        if self.is_running() {
            self.send_sigkill();
        }
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
