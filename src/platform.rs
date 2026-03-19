use std::io::Write;
use std::process::{Command, Stdio};

pub fn open_url(url: &str) -> Result<(), String> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "linux") {
        "xdg-open"
    } else {
        return Err("unsupported platform".to_string());
    };

    Command::new(cmd)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    Ok(())
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        child.wait().map_err(|e| e.to_string())?;
        Ok(())
    } else if cfg!(target_os = "linux") {
        let result = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn();
        let mut child = match result {
            Ok(child) => child,
            Err(_) => Command::new("xsel")
                .arg("--clipboard")
                .stdin(Stdio::piped())
                .spawn()
                .map_err(|e| format!("neither xclip nor xsel available: {}", e))?,
        };
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        child.wait().map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("unsupported platform".to_string())
    }
}
