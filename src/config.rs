use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub grafana: GrafanaConfig,
    pub loki: LokiConfig,
    pub prometheus: Option<PrometheusConfig>,
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
    pub datasource: Option<String>,
    #[serde(default = "default_limit")]
    pub default_limit: u32,
    #[serde(default = "max_limit")]
    pub max_limit: u32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PrometheusConfig {
    pub datasource: Option<String>,
    #[serde(default = "default_target_points")]
    pub target_points: u32,
    #[serde(default = "default_max_series")]
    pub max_series: u32,
    #[serde(default = "default_min_step_seconds")]
    pub min_step_seconds: u32,
}

fn default_limit() -> u32 {
    200
}

fn max_limit() -> u32 {
    1000
}

fn default_target_points() -> u32 {
    100
}

fn default_max_series() -> u32 {
    20
}

fn default_min_step_seconds() -> u32 {
    15
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
        let mut config = Self::from_str(&contents)?;
        config.apply_env_overrides();
        Ok(config)
    }

    // MIRU_API_KEY takes precedence over the token in the file, so the secret
    // can come from the environment instead of living on disk.
    fn apply_env_overrides(&mut self) {
        if let Ok(key) = std::env::var("MIRU_API_KEY")
            && !key.is_empty()
        {
            self.grafana.api_key = Some(key);
        }
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
    fn env_var_overrides_and_sets_api_key() {
        unsafe {
            std::env::set_var("MIRU_API_KEY", "env-key");
        }

        // overrides a value from the file
        let mut from_file = Config::from_str(full_toml()).unwrap();
        from_file.apply_env_overrides();
        assert_eq!(from_file.grafana.api_key, Some("env-key".to_string()));

        // sets the key when the file has none
        let mut without_key = Config::from_str(no_api_key_toml()).unwrap();
        without_key.apply_env_overrides();

        unsafe {
            std::env::remove_var("MIRU_API_KEY");
        }
        assert_eq!(without_key.grafana.api_key, Some("env-key".to_string()));
    }

    #[test]
    fn no_prometheus_section_is_none() {
        let cfg = Config::from_str(minimal_toml()).unwrap();
        assert!(cfg.prometheus.is_none());
    }

    #[test]
    fn parses_prometheus_section_with_defaults() {
        let cfg = Config::from_str(
            r#"
[grafana]
url = "http://localhost:3000"
[loki]
service_label = "app"
[prometheus]
datasource = "Prometheus"
"#,
        )
        .unwrap();
        let prom = cfg.prometheus.unwrap();
        assert_eq!(prom.datasource, Some("Prometheus".to_string()));
        assert_eq!(prom.target_points, 100);
        assert_eq!(prom.max_series, 20);
        assert_eq!(prom.min_step_seconds, 15);
    }

    #[test]
    fn parses_prometheus_overrides() {
        let cfg = Config::from_str(
            r#"
[grafana]
url = "http://localhost:3000"
[loki]
service_label = "app"
[prometheus]
target_points = 250
max_series = 5
min_step_seconds = 30
"#,
        )
        .unwrap();
        let prom = cfg.prometheus.unwrap();
        assert_eq!(prom.datasource, None);
        assert_eq!(prom.target_points, 250);
        assert_eq!(prom.max_series, 5);
        assert_eq!(prom.min_step_seconds, 30);
    }

    #[test]
    fn parses_loki_datasource() {
        let cfg = Config::from_str(
            r#"
[grafana]
url = "http://localhost:3000"
[loki]
service_label = "app"
datasource = "LokiProd"
"#,
        )
        .unwrap();
        assert_eq!(cfg.loki.datasource, Some("LokiProd".to_string()));
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
