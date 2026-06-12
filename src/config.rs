use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub grafana: GrafanaConfig,
    pub loki: LokiConfig,
}

#[derive(Deserialize)]
pub struct GrafanaConfig {
    pub url: String,
    pub api_key: Option<String>,
    pub username: Option<String>,
    pub datasource: Option<String>,
}

impl std::fmt::Debug for GrafanaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrafanaConfig")
            .field("url", &self.url)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("username", &self.username)
            .field("datasource", &self.datasource)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct LokiConfig {
    pub service_label: String,
    pub level_label: Option<String>,
    #[serde(default = "default_limit")]
    pub default_limit: u32,
    #[serde(default = "max_limit")]
    pub max_limit: u32,
}

fn default_limit() -> u32 {
    200
}

fn max_limit() -> u32 {
    1000
}

impl Config {
    pub fn from_str(s: &str) -> Result<Self> {
        toml::from_str(s).context("failed to parse config TOML")
    }

    pub fn load() -> Result<Self> {
        let path = config_path();
        let contents = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "config file not found at {}. Copy config.example.toml to that path and fill it in.",
                path.display()
            )
        })?;
        let config = Self::from_str(&contents)?;
        Ok(config)
    }

}

impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.grafana.url.is_empty() {
            anyhow::bail!("grafana.url is required");
        }
        if !self.grafana.url.starts_with("http://") && !self.grafana.url.starts_with("https://") {
            anyhow::bail!(
                "grafana.url must start with http:// or https:// (got: {})",
                self.grafana.url
            );
        }
        if self.loki.service_label.is_empty() {
            anyhow::bail!("loki.service_label is required");
        }
        Ok(())
    }
}

fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("MIRU_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("miru")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_toml() -> &'static str {
        r#"
[grafana]
url = "http://localhost:3000"
api_key = "test-key"

[loki]
service_label = "app"
default_limit = 100
max_limit = 500
"#
    }

    fn minimal_toml() -> &'static str {
        r#"
[grafana]
url = "http://localhost:3000"
api_key = "test-key"

[loki]
service_label = "job"
"#
    }

    #[test]
    fn parses_full_config() {
        let cfg = Config::from_str(full_toml()).unwrap();
        assert_eq!(cfg.grafana.url, "http://localhost:3000");
        assert_eq!(cfg.grafana.api_key, Some("test-key".to_string()));
        assert_eq!(cfg.loki.service_label, "app");
        assert_eq!(cfg.loki.default_limit, 100);
        assert_eq!(cfg.loki.max_limit, 500);
    }

    #[test]
    fn applies_defaults_for_limits() {
        let cfg = Config::from_str(minimal_toml()).unwrap();
        assert_eq!(cfg.loki.default_limit, 200);
        assert_eq!(cfg.loki.max_limit, 1000);
    }

    #[test]
    fn rejects_malformed_toml() {
        let result = Config::from_str("not valid toml {{{{");
        assert!(result.is_err());
    }

    #[test]
    fn respects_miru_config_env_var() {
        unsafe {
            std::env::set_var("MIRU_CONFIG", "/tmp/custom_miru_test.toml");
        }
        let path = config_path();
        unsafe {
            std::env::remove_var("MIRU_CONFIG");
        }
        assert_eq!(path, std::path::PathBuf::from("/tmp/custom_miru_test.toml"));
    }

    fn optional_fields_toml() -> &'static str {
        r#"
[grafana]
url = "http://localhost:3000"
api_key = "test-key"
username = "123456"
datasource = "LokiProd"

[loki]
service_label = "app"
"#
    }

    fn no_api_key_toml() -> &'static str {
        r#"
[grafana]
url = "http://localhost:3100"

[loki]
service_label = "job"
"#
    }

    #[test]
    fn parses_optional_grafana_fields() {
        let cfg = Config::from_str(optional_fields_toml()).unwrap();
        assert_eq!(cfg.grafana.username, Some("123456".to_string()));
        assert_eq!(cfg.grafana.datasource, Some("LokiProd".to_string()));
    }

    #[test]
    fn parses_config_without_api_key() {
        let cfg = Config::from_str(no_api_key_toml()).unwrap();
        assert_eq!(cfg.grafana.api_key, None);
        assert_eq!(cfg.grafana.username, None);
    }


    #[test]
    fn parses_level_label() {
        let cfg = Config::from_str(
            r#"
[grafana]
url = "http://localhost:3000"
[loki]
service_label = "app"
level_label = "level"
"#,
        )
        .unwrap();
        assert_eq!(cfg.loki.level_label, Some("level".to_string()));
    }

    #[test]
    fn validate_rejects_empty_url() {
        let cfg = Config::from_str(
            r#"
[grafana]
url = ""
[loki]
service_label = "app"
"#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("grafana.url is required"), "got: {err}");
    }

    #[test]
    fn validate_rejects_non_http_url() {
        let cfg = Config::from_str(
            r#"
[grafana]
url = "grpc://localhost:3000"
[loki]
service_label = "app"
"#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("http://") || err.contains("https://"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_rejects_empty_service_label() {
        let cfg = Config::from_str(
            r#"
[grafana]
url = "http://localhost:3000"
[loki]
service_label = ""
"#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("service_label"), "got: {err}");
    }
}
