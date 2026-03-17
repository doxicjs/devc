use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub general: General,
    pub services: Vec<ServiceConfig>,
    #[serde(default)]
    pub links: Vec<LinkConfig>,
    #[serde(default)]
    pub copies: Vec<CopyConfig>,
}

#[derive(Debug, Deserialize)]
pub struct General {
    pub project_root: String,
}

#[derive(Debug, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub key: String,
    pub command: String,
    pub working_dir: String,
    #[allow(dead_code)]
    pub service_type: String,
    pub port: Option<u16>,
    pub url: Option<String>,
}

impl ServiceConfig {
    pub fn key_char(&self) -> char {
        self.key.chars().next().unwrap_or('?')
    }

    pub fn open_url(&self) -> Option<String> {
        self.url
            .clone()
            .or_else(|| self.port.map(|p| format!("http://localhost:{}/", p)))
    }

    pub fn full_command(&self) -> String {
        let mut cmd = self.command.clone();
        if let Some(port) = self.port {
            cmd.push_str(&format!(" --port {}", port));
        }
        cmd
    }
}

#[derive(Debug, Deserialize)]
pub struct LinkConfig {
    pub name: String,
    pub key: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct CopyConfig {
    pub name: String,
    pub key: String,
    pub text: String,
}
