use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

pub struct ProcessHandle {
    child: Child,
}

impl ProcessHandle {
    pub fn spawn(
        command: &str,
        working_dir: &str,
        log_sender: mpsc::Sender<(usize, String)>,
        service_idx: usize,
    ) -> Result<Self, String> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let mut child = cmd.spawn().map_err(|e| e.to_string())?;

        if let Some(stdout) = child.stdout.take() {
            let sender = log_sender.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().flatten() {
                    if sender.send((service_idx, line)).is_err() {
                        break;
                    }
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let sender = log_sender;
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    if sender.send((service_idx, line)).is_err() {
                        break;
                    }
                }
            });
        }

        Ok(Self { child })
    }

    pub fn kill(&mut self) {
        let pid = self.child.id() as i32;
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
        // Give processes time to shut down gracefully
        std::thread::sleep(std::time::Duration::from_millis(500));
        if self.is_running() {
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
        let _ = self.child.wait();
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}
