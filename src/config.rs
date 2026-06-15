//! Configuration model for automation policy, notification defaults, and lifecycle tuning.

use std::path::PathBuf;

use serde::Deserialize;

use crate::{
    model::{DurationSpec, Lead},
    store::Store,
};

/// Application configuration loaded from `<config>/ccplan/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub automation: AutomationConfig,
    pub notify: NotifyConfig,
    pub grace: DurationSpec,
}

impl Config {
    /// Loads configuration from the store's config path, returning defaults if the file is absent.
    ///
    /// # Errors
    ///
    /// Returns an error if the config file exists but cannot be parsed.
    pub fn load(store: &Store) -> Result<Self, ConfigError> {
        let path = store.config_path();
        match std::fs::read_to_string(&path) {
            Ok(input) => {
                let raw: RawConfig =
                    toml::from_str(&input).map_err(|source| ConfigError::Parse {
                        path: path.clone(),
                        source,
                    })?;
                Ok(raw.into())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io { path, source }),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            automation: AutomationConfig::default(),
            notify: NotifyConfig::default(),
            grace: DEFAULT_GRACE,
        }
    }
}

/// Automation policy: whether `run:` is enabled, which executables are allowed, and timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationConfig {
    pub enabled: bool,
    pub allowed_executables: Vec<PathBuf>,
    pub timeout: DurationSpec,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_executables: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// Notification defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyConfig {
    pub default_lead: Lead,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            default_lead: DEFAULT_LEAD,
        }
    }
}

// 90s default grace — matches the hardcoded value from context.rs
const DEFAULT_GRACE: DurationSpec = match DurationSpec::from_seconds_const(90) {
    Some(d) => d,
    None => unreachable!(),
};

// 5m default timeout for run: commands
const DEFAULT_TIMEOUT: DurationSpec = match DurationSpec::from_seconds_const(300) {
    Some(d) => d,
    None => unreachable!(),
};

// 5m default notification lead
const DEFAULT_LEAD: Lead = Lead::from_seconds_const(300);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    automation: RawAutomationConfig,
    #[serde(default)]
    notify: RawNotifyConfig,
    #[serde(default = "default_grace_str")]
    grace: String,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            automation: RawAutomationConfig::default(),
            notify: RawNotifyConfig::default(),
            grace: default_grace_str(),
        }
    }
}

fn default_grace_str() -> String {
    "90s".to_owned()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAutomationConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    allowed_executables: Vec<PathBuf>,
    #[serde(default = "default_timeout_str")]
    timeout: String,
}

fn default_timeout_str() -> String {
    "5m".to_owned()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNotifyConfig {
    #[serde(default = "default_lead_str")]
    default_lead: String,
}

fn default_lead_str() -> String {
    "5m".to_owned()
}

impl From<RawConfig> for Config {
    fn from(raw: RawConfig) -> Self {
        let timeout = raw
            .automation
            .timeout
            .parse::<DurationSpec>()
            .unwrap_or(DEFAULT_TIMEOUT);
        let default_lead = raw
            .notify
            .default_lead
            .parse::<Lead>()
            .unwrap_or(DEFAULT_LEAD);
        let grace = raw.grace.parse::<DurationSpec>().unwrap_or(DEFAULT_GRACE);

        Self {
            automation: AutomationConfig {
                enabled: raw.automation.enabled,
                allowed_executables: raw.automation.allowed_executables,
                timeout,
            },
            notify: NotifyConfig { default_lead },
            grace,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(
                formatter,
                "config I/O error at `{}`: {source}",
                path.display()
            ),
            Self::Parse { path, source } => {
                write!(
                    formatter,
                    "config parse error at `{}`: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use assert_fs::TempDir;

    fn test_store() -> (TempDir, Store) {
        let temp = TempDir::new().unwrap();
        let store = Store::new(temp.path());
        (temp, store)
    }

    #[test]
    fn default_config_when_file_is_missing() {
        let (_temp, store) = test_store();
        let config = Config::load(&store).unwrap();

        assert!(!config.automation.enabled);
        assert!(config.automation.allowed_executables.is_empty());
        assert_eq!(config.automation.timeout.as_seconds(), 300);
        assert_eq!(config.notify.default_lead.as_seconds(), 300);
        assert_eq!(config.grace.as_seconds(), 90);
    }

    #[test]
    fn parses_valid_config_file() {
        let (_temp, store) = test_store();
        let config_path = store.config_path();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            r#"
grace = "2m"

[automation]
enabled = true
allowed_executables = ["/usr/bin/echo", "/usr/local/bin/sync.sh"]
timeout = "10m"

[notify]
default_lead = "3m"
"#,
        )
        .unwrap();

        let config = Config::load(&store).unwrap();

        assert!(config.automation.enabled);
        assert_eq!(config.automation.allowed_executables.len(), 2);
        assert_eq!(
            config.automation.allowed_executables[0],
            PathBuf::from("/usr/bin/echo")
        );
        assert_eq!(config.automation.timeout.as_seconds(), 600);
        assert_eq!(config.notify.default_lead.as_seconds(), 180);
        assert_eq!(config.grace.as_seconds(), 120);
    }

    #[test]
    fn rejects_invalid_config_toml() {
        let (_temp, store) = test_store();
        let config_path = store.config_path();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "not valid [[[ toml").unwrap();

        let result = Config::load(&store);

        assert!(result.is_err());
        let error = result.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("config parse error"));
    }

    #[test]
    fn rejects_unknown_config_fields() {
        let (_temp, store) = test_store();
        let config_path = store.config_path();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "unknown_field = true\n").unwrap();

        let result = Config::load(&store);

        assert!(result.is_err());
    }

    #[test]
    fn io_error_when_config_path_is_directory() {
        let (_temp, store) = test_store();
        let config_path = store.config_path();
        std::fs::create_dir_all(&config_path).unwrap();

        let result = Config::load(&store);

        assert!(result.is_err());
        let error = result.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("config I/O error"));
    }

    #[test]
    fn default_values_for_empty_config() {
        let (_temp, store) = test_store();
        let config_path = store.config_path();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "").unwrap();

        let config = Config::load(&store).unwrap();

        assert!(!config.automation.enabled);
        assert_eq!(config.automation.timeout.as_seconds(), 300);
        assert_eq!(config.notify.default_lead.as_seconds(), 300);
        assert_eq!(config.grace.as_seconds(), 90);
    }

    #[test]
    fn raw_config_default_and_helpers() {
        let rc = RawConfig::default();
        assert_eq!(rc.grace, "90s");
        assert_eq!(default_timeout_str(), "5m");
        assert_eq!(default_lead_str(), "5m");
    }
}
