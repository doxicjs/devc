use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    #[serde(default)]
    pub commands: Vec<CommandConfig>,
    #[serde(default)]
    pub links: Vec<LinkConfig>,
    #[serde(default)]
    pub copies: Vec<CopyConfig>,
}

fn default_project_root() -> String {
    "./".to_string()
}

#[derive(Debug, Deserialize)]
pub struct General {
    #[serde(default = "default_project_root")]
    pub project_root: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            project_root: default_project_root(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub name: String,
    pub key: String,
    pub command: String,
    pub working_dir: String,
    #[allow(dead_code)]
    pub service_type: String,
    #[serde(default, deserialize_with = "deserialize_port")]
    pub port: Option<u16>,
    pub url: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

fn deserialize_port<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let port: Option<u16> = Option::deserialize(deserializer)?;
    if let Some(p) = port {
        if p == 0 {
            return Err(serde::de::Error::custom("port must be between 1 and 65535"));
        }
    }
    Ok(port)
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
        self.command.clone()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    pub name: String,
    pub key: String,
    pub command: String,
    pub working_dir: String,
}

impl CommandConfig {
    pub fn key_char(&self) -> char {
        self.key.chars().next().unwrap_or('?')
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    pub name: String,
    pub key: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyConfig {
    pub name: String,
    pub key: String,
    pub text: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct LocalConfig {
    #[serde(default)]
    pub general: Option<LocalGeneral>,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    #[serde(default)]
    pub commands: Vec<CommandConfig>,
    #[serde(default)]
    pub links: Vec<LinkConfig>,
    #[serde(default)]
    pub copies: Vec<CopyConfig>,
}

#[derive(Debug, Default, Deserialize)]
pub struct LocalGeneral {
    #[serde(default)]
    pub project_root: Option<String>,
}

impl Config {
    pub fn merge_local(&mut self, local: LocalConfig) {
        if let Some(general) = local.general {
            if let Some(project_root) = general.project_root {
                self.general.project_root = project_root;
            }
        }
        merge_by_name(&mut self.services, local.services, |s| &s.name);
        merge_by_name(&mut self.commands, local.commands, |c| &c.name);
        merge_by_name(&mut self.links, local.links, |l| &l.name);
        merge_by_name(&mut self.copies, local.copies, |c| &c.name);
    }
}

fn merge_by_name<T, F>(base: &mut Vec<T>, overlay: Vec<T>, name_of: F)
where
    F: Fn(&T) -> &String,
{
    for item in overlay {
        if let Some(existing) = base.iter_mut().find(|b| name_of(b) == name_of(&item)) {
            *existing = item;
        } else {
            base.push(item);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str, key: &str, port: Option<u16>) -> ServiceConfig {
        ServiceConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: format!("echo {}", name),
            working_dir: "./".to_string(),
            service_type: "generic".to_string(),
            port,
            url: None,
            depends_on: vec![],
        }
    }

    fn svc(name: &str, key: &str, command: &str) -> ServiceConfig {
        ServiceConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: command.to_string(),
            working_dir: ".".to_string(),
            service_type: "backend".to_string(),
            port: None,
            url: None,
            depends_on: vec![],
        }
    }

    // ===== Issue #2: services field should be optional in TOML =====

    #[test]
    fn parse_config_without_services_section() {
        let toml_str = r#"
[general]
project_root = "./"

[[commands]]
name = "build"
key = "b"
command = "cargo build"
working_dir = "./"
"#;
        let config: Result<Config, _> = toml::from_str(toml_str);
        assert!(
            config.is_ok(),
            "Config without [[services]] should parse: {:?}",
            config.err()
        );
        assert!(config.unwrap().services.is_empty());
    }

    #[test]
    fn parse_minimal_config_only_general() {
        let toml_str = r#"
[general]
project_root = "./"
"#;
        let config: Result<Config, _> = toml::from_str(toml_str);
        assert!(
            config.is_ok(),
            "Config with only [general] should parse: {:?}",
            config.err()
        );
        let c = config.unwrap();
        assert!(c.services.is_empty());
        assert!(c.commands.is_empty());
        assert!(c.links.is_empty());
        assert!(c.copies.is_empty());
    }

    #[test]
    fn parse_empty_toml() {
        let config: Result<Config, _> = toml::from_str("");
        assert!(
            config.is_ok(),
            "Empty TOML should parse with all defaults: {:?}",
            config.err()
        );
    }

    #[test]
    fn parse_config_with_only_links_and_copies() {
        let toml_str = r#"
[[links]]
name = "Docs"
key = "d"
url = "https://docs.example.com"

[[copies]]
name = "Token"
key = "t"
text = "abc123"
"#;
        let config: Result<Config, _> = toml::from_str(toml_str);
        assert!(config.is_ok(), "Config with only links/copies should parse: {:?}", config.err());
        let c = config.unwrap();
        assert!(c.services.is_empty());
        assert!(c.commands.is_empty());
        assert_eq!(c.links.len(), 1);
        assert_eq!(c.copies.len(), 1);
    }

    // ===== Issue #5: full_command should NOT auto-append --port =====

    #[test]
    fn full_command_does_not_append_port() {
        let svc = service("web", "w", Some(3000));
        assert_eq!(
            svc.full_command(),
            "echo web",
            "full_command() should return command as-is, not append --port"
        );
    }

    #[test]
    fn full_command_without_port_returns_as_is() {
        let svc = service("worker", "k", None);
        assert_eq!(svc.full_command(), "echo worker");
    }

    // ===== key_char edge cases =====

    #[test]
    fn key_char_returns_first_character() {
        let svc = service("web", "w", None);
        assert_eq!(svc.key_char(), 'w');
    }

    #[test]
    fn key_char_empty_string_returns_fallback() {
        let mut svc = service("web", "w", None);
        svc.key = String::new();
        assert_eq!(svc.key_char(), '?');
    }

    #[test]
    fn key_char_multichar_uses_first_only() {
        let svc = service("web", "abc", None);
        assert_eq!(svc.key_char(), 'a');
    }

    #[test]
    fn command_config_key_char_basic() {
        let cmd = CommandConfig {
            name: "build".to_string(),
            key: "b".to_string(),
            command: "cargo build".to_string(),
            working_dir: "./".to_string(),
        };
        assert_eq!(cmd.key_char(), 'b');
    }

    #[test]
    fn command_config_key_char_empty() {
        let cmd = CommandConfig {
            name: "build".to_string(),
            key: String::new(),
            command: "cargo build".to_string(),
            working_dir: "./".to_string(),
        };
        assert_eq!(cmd.key_char(), '?');
    }

    // ===== open_url =====

    #[test]
    fn open_url_prefers_explicit_url() {
        let mut svc = service("web", "w", Some(3000));
        svc.url = Some("https://custom.dev".to_string());
        assert_eq!(svc.open_url(), Some("https://custom.dev".to_string()));
    }

    #[test]
    fn open_url_falls_back_to_localhost_port() {
        let svc = service("web", "w", Some(8080));
        assert_eq!(svc.open_url(), Some("http://localhost:8080/".to_string()));
    }

    #[test]
    fn open_url_returns_none_without_url_or_port() {
        let svc = service("worker", "k", None);
        assert_eq!(svc.open_url(), None);
    }

    // ===== Deserialization defaults =====

    #[test]
    fn service_config_defaults_depends_on_to_empty() {
        let toml_str = r#"
[[services]]
name = "web"
key = "w"
command = "echo hi"
working_dir = "./"
service_type = "node"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.services[0].depends_on.is_empty());
    }

    #[test]
    fn general_defaults_project_root() {
        let toml_str = r#"
[[services]]
name = "web"
key = "w"
command = "echo hi"
working_dir = "./"
service_type = "node"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.project_root, "./");
    }

    // ===== Fix 2: deny_unknown_fields catches typos =====

    #[test]
    fn unknown_service_field_rejected() {
        let toml_str = r#"
[[services]]
name = "web"
key = "w"
command = "echo hi"
working_dir = "./"
service_type = "node"
poort = 3000
"#;
        let config: Result<Config, _> = toml::from_str(toml_str);
        assert!(config.is_err(), "Typo 'poort' should be rejected");
    }

    #[test]
    fn unknown_command_field_rejected() {
        let toml_str = r#"
[[commands]]
name = "build"
key = "b"
comand = "cargo build"
working_dir = "./"
"#;
        let config: Result<Config, _> = toml::from_str(toml_str);
        assert!(config.is_err(), "Typo 'comand' should be rejected");
    }

    // ===== Fix 2: port validation =====

    #[test]
    fn port_zero_rejected() {
        let toml_str = r#"
[[services]]
name = "web"
key = "w"
command = "echo hi"
working_dir = "./"
service_type = "node"
port = 0
"#;
        let config: Result<Config, _> = toml::from_str(toml_str);
        assert!(config.is_err(), "Port 0 should be rejected");
    }

    // ===== Local override merge =====

    fn cmd(name: &str, key: &str, command: &str) -> CommandConfig {
        CommandConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: command.to_string(),
            working_dir: ".".to_string(),
        }
    }

    fn link(name: &str, key: &str, url: &str) -> LinkConfig {
        LinkConfig {
            name: name.to_string(),
            key: key.to_string(),
            url: url.to_string(),
        }
    }

    fn copy(name: &str, key: &str, text: &str) -> CopyConfig {
        CopyConfig {
            name: name.to_string(),
            key: key.to_string(),
            text: text.to_string(),
        }
    }

    fn base_config() -> Config {
        Config {
            general: General {
                project_root: "./main".to_string(),
            },
            services: vec![svc("API", "a", "api"), svc("Web", "w", "web")],
            commands: vec![cmd("Migrate", "m", "migrate")],
            links: vec![link("Docs", "d", "https://docs")],
            copies: vec![copy("Token", "t", "secret")],
        }
    }

    #[test]
    fn merge_local_appends_new_items() {
        let mut config = base_config();
        let local = LocalConfig {
            services: vec![svc("Scratch", "s", "scratch")],
            commands: vec![cmd("Seed", "e", "seed")],
            links: vec![link("Local Admin", "l", "http://localhost:9000")],
            copies: vec![copy("Dev Key", "k", "dev")],
            ..Default::default()
        };

        config.merge_local(local);

        assert_eq!(config.services.len(), 3);
        assert_eq!(config.services[2].name, "Scratch");
        assert_eq!(config.commands.len(), 2);
        assert_eq!(config.commands[1].name, "Seed");
        assert_eq!(config.links.len(), 2);
        assert_eq!(config.links[1].name, "Local Admin");
        assert_eq!(config.copies.len(), 2);
        assert_eq!(config.copies[1].name, "Dev Key");
    }

    #[test]
    fn merge_local_overrides_by_name() {
        let mut config = base_config();
        let local = LocalConfig {
            services: vec![svc("Web", "w", "pnpm dev --debug")],
            ..Default::default()
        };

        config.merge_local(local);

        assert_eq!(config.services.len(), 2);
        assert_eq!(config.services[0].name, "API");
        assert_eq!(config.services[1].name, "Web");
        assert_eq!(config.services[1].command, "pnpm dev --debug");
    }

    #[test]
    fn merge_local_general_field_by_field() {
        let mut config = base_config();
        let local = LocalConfig {
            general: Some(LocalGeneral {
                project_root: Some("./override".to_string()),
            }),
            ..Default::default()
        };

        config.merge_local(local);

        assert_eq!(config.general.project_root, "./override");
    }

    #[test]
    fn merge_local_general_absent_leaves_main_untouched() {
        let mut config = base_config();
        let local = LocalConfig {
            general: Some(LocalGeneral {
                project_root: None,
            }),
            ..Default::default()
        };

        config.merge_local(local);

        assert_eq!(config.general.project_root, "./main");
    }

    #[test]
    fn merge_local_empty_is_noop() {
        let mut config = base_config();
        config.merge_local(LocalConfig::default());

        assert_eq!(config.general.project_root, "./main");
        assert_eq!(config.services.len(), 2);
        assert_eq!(config.commands.len(), 1);
        assert_eq!(config.links.len(), 1);
        assert_eq!(config.copies.len(), 1);
    }

    #[test]
    fn merge_local_section_name_isolation() {
        let mut config = base_config();
        let local = LocalConfig {
            services: vec![svc("Migrate", "g", "different-service")],
            commands: vec![cmd("Migrate", "m", "migrate-v2")],
            ..Default::default()
        };

        config.merge_local(local);

        assert_eq!(config.services.len(), 3);
        assert!(config.services.iter().any(|s| s.name == "Migrate"
            && s.command == "different-service"));
        assert_eq!(config.commands.len(), 1);
        assert_eq!(config.commands[0].command, "migrate-v2");
    }
}
