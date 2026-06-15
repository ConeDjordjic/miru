use anyhow::{Result, bail};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

use crate::config::GrafanaConfig;

#[derive(Clone, Debug)]
pub enum Auth {
    None,
    Bearer(String),
    Basic { username: String, password: String },
}

impl Auth {
    pub fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Auth::None => req,
            Auth::Bearer(token) => req.bearer_auth(token),
            Auth::Basic { username, password } => req.basic_auth(username, Some(password)),
        }
    }
}

fn map_http_error(status: u16) -> String {
    match status {
        401 => "authentication failed".into(),
        403 => "access denied".into(),
        404 => "endpoint not found".into(),
        s if s >= 500 => format!("server error (HTTP {s})"),
        s => format!("HTTP {s}"),
    }
}

fn truncate_body(body: &str) -> String {
    const MAX: usize = 500;
    if body.chars().count() > MAX {
        let head: String = body.chars().take(MAX).collect();
        format!("{head} (truncated)")
    } else {
        body.to_string()
    }
}

pub struct Endpoint {
    base_url: String,
    auth: Auth,
    name: &'static str,
    http: Client,
}

impl Endpoint {
    pub fn new(base_url: &str, auth: Auth, name: &'static str) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth,
            name,
            http,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn get_json<T, Q>(&self, path: &str, params: &Q) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
        Q: serde::Serialize + ?Sized,
    {
        let resp = self
            .auth
            .apply(self.http.get(self.url(path)))
            .query(params)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    anyhow::anyhow!("cannot connect to {}: {e}", self.name)
                } else {
                    anyhow::anyhow!("{e}")
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let s = status.as_u16();
            if matches!(s, 401 | 403 | 404) || s >= 500 {
                bail!("{}", map_http_error(s));
            }
            let body = resp.text().await.unwrap_or_default();
            bail!("{} returned HTTP {s}: {}", self.name, truncate_body(&body));
        }

        Ok(resp.json().await?)
    }
}

#[derive(Debug)]
pub struct ResolvedBackend {
    pub base_url: String,
    pub auth: Auth,
}

#[derive(Deserialize)]
struct GrafanaHealth {
    database: String,
}

#[derive(Deserialize)]
struct Datasource {
    uid: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

fn build_auth(config: &GrafanaConfig) -> Result<Auth> {
    match (&config.username, &config.api_key) {
        (Some(u), Some(k)) => Ok(Auth::Basic {
            username: u.clone(),
            password: k.clone(),
        }),
        (Some(_), None) => bail!("`username` requires `api_key` to be set"),
        (None, Some(k)) => Ok(Auth::Bearer(k.clone())),
        (None, None) => Ok(Auth::None),
    }
}

pub async fn resolve(config: &GrafanaConfig) -> Result<ResolvedBackend> {
    let auth = build_auth(config)?;
    let http = Client::builder().timeout(Duration::from_secs(10)).build()?;

    let base = config.url.trim_end_matches('/');

    // If it's Grafana, route through the datasource proxy.
    if let Ok(resp) = auth
        .apply(http.get(format!("{base}/api/health")))
        .send()
        .await
        && resp.status().is_success()
        && let Ok(health) = resp.json::<GrafanaHealth>().await
        && health.database == "ok"
    {
        return resolve_grafana(&http, config, base, auth).await;
    }

    // Otherwise try Loki directly.
    if let Ok(resp) = auth
        .apply(http.get(format!("{base}/loki/api/v1/labels")))
        .send()
        .await
        && resp.status().is_success()
    {
        return Ok(ResolvedBackend {
            base_url: base.to_string(),
            auth,
        });
    }

    bail!("could not connect to Grafana or Loki at {base}")
}

async fn resolve_grafana(
    http: &Client,
    config: &GrafanaConfig,
    base: &str,
    auth: Auth,
) -> Result<ResolvedBackend> {
    let resp = auth
        .apply(http.get(format!("{base}/api/datasources")))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Grafana datasources API returned {status}: {body}");
    }

    let all: Vec<Datasource> = resp.json().await?;
    let loki: Vec<Datasource> = all.into_iter().filter(|ds| ds.kind == "loki").collect();

    let selected = match &config.datasource {
        Some(name) => loki
            .into_iter()
            .find(|ds| &ds.name == name)
            .ok_or_else(|| anyhow::anyhow!("no Loki datasource named {name:?} found in Grafana"))?,
        None => loki
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no Loki datasource found in Grafana"))?,
    };

    Ok(ResolvedBackend {
        base_url: format!("{base}/api/datasources/proxy/uid/{}", selected.uid),
        auth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn bearer_config(url: &str) -> GrafanaConfig {
        GrafanaConfig {
            url: url.to_string(),
            api_key: Some("test-token".into()),
            username: None,
            datasource: None,
        }
    }

    fn grafana_health_body() -> &'static str {
        r#"{"database":"ok","version":"10.0.0","commit":"abc"}"#
    }

    #[test]
    fn build_auth_basic() {
        let config = GrafanaConfig {
            url: "http://x".into(),
            api_key: Some("pass".into()),
            username: Some("user".into()),
            datasource: None,
        };
        assert!(matches!(
            build_auth(&config).unwrap(),
            Auth::Basic { username, password } if username == "user" && password == "pass"
        ));
    }

    #[test]
    fn build_auth_username_without_api_key_errors() {
        let config = GrafanaConfig {
            url: "http://x".into(),
            api_key: None,
            username: Some("user".into()),
            datasource: None,
        };
        assert!(build_auth(&config).is_err());
    }

    #[tokio::test]
    async fn endpoint_connect_error_names_the_backend() {
        let endpoint = Endpoint::new("http://127.0.0.1:1", Auth::None, "Prometheus");
        let err = endpoint
            .get_json::<serde_json::Value, _>("/api/v1/query", &[("query", "up")])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot connect to Prometheus"), "got: {err}");
    }

    #[tokio::test]
    async fn endpoint_maps_404_to_friendly_message() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", mockito::Matcher::Any)
            .with_status(404)
            .create_async()
            .await;
        let endpoint = Endpoint::new(&server.url(), Auth::None, "Prometheus");
        let err = endpoint
            .get_json::<serde_json::Value, _>("/api/v1/query", &[("query", "up")])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("endpoint not found"), "got: {err}");
    }

    #[tokio::test]
    async fn detects_grafana_resolves_first_loki_datasource() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/api/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(grafana_health_body())
            .create_async()
            .await;
        server
            .mock("GET", "/api/datasources")
            .match_header("authorization", "Bearer test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"uid":"uid1","name":"Loki","type":"loki"}]"#)
            .create_async()
            .await;

        let backend = resolve(&bearer_config(&server.url())).await.unwrap();
        assert_eq!(
            backend.base_url,
            format!("{}/api/datasources/proxy/uid/uid1", server.url())
        );
    }

    #[tokio::test]
    async fn selects_named_datasource() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/api/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(grafana_health_body())
            .create_async()
            .await;
        server
            .mock("GET", "/api/datasources")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"uid":"uid1","name":"Loki","type":"loki"},{"uid":"uid2","name":"LokiProd","type":"loki"}]"#)
            .create_async()
            .await;

        let config = GrafanaConfig {
            url: server.url(),
            api_key: Some("test-token".into()),
            username: None,
            datasource: Some("LokiProd".into()),
        };
        let backend = resolve(&config).await.unwrap();
        assert_eq!(
            backend.base_url,
            format!("{}/api/datasources/proxy/uid/uid2", server.url())
        );
    }

    #[tokio::test]
    async fn errors_when_named_datasource_not_found() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/api/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(grafana_health_body())
            .create_async()
            .await;
        server
            .mock("GET", "/api/datasources")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"uid":"uid1","name":"Loki","type":"loki"}]"#)
            .create_async()
            .await;

        let config = GrafanaConfig {
            url: server.url(),
            api_key: Some("test-token".into()),
            username: None,
            datasource: Some("Missing".into()),
        };
        let err = resolve(&config).await.unwrap_err();
        assert!(err.to_string().contains("Missing"));
    }

    #[tokio::test]
    async fn falls_back_to_direct_loki_when_grafana_probe_fails() {
        let mut server = Server::new_async().await;
        // No /api/health mock, so mockito returns 501 and Grafana detection fails.
        server
            .mock("GET", "/loki/api/v1/labels")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"success","data":["app"]}"#)
            .create_async()
            .await;

        let backend = resolve(&bearer_config(&server.url())).await.unwrap();
        assert_eq!(
            backend.base_url,
            server.url().trim_end_matches('/').to_string()
        );
    }

    #[tokio::test]
    async fn errors_when_both_probes_fail() {
        let server = Server::new_async().await;
        // No mocks, so both probes return 501.
        assert!(resolve(&bearer_config(&server.url())).await.is_err());
    }

    #[tokio::test]
    async fn basic_auth_is_sent_in_probe_requests() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/api/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(grafana_health_body())
            .create_async()
            .await;
        server
            .mock("GET", "/api/datasources")
            .match_header("authorization", mockito::Matcher::Regex("Basic .+".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"uid":"uid1","name":"Loki","type":"loki"}]"#)
            .create_async()
            .await;

        let config = GrafanaConfig {
            url: server.url(),
            api_key: Some("token".into()),
            username: Some("123".into()),
            datasource: None,
        };
        let backend = resolve(&config).await.unwrap();
        assert!(matches!(backend.auth, Auth::Basic { .. }));
    }
}
