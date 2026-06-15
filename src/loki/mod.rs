pub mod query;

use anyhow::{Result, bail};
use chrono::Utc;
use query::{LogQuery, LogRequest};
use serde::Deserialize;
use std::time::Duration;

fn format_ns_timestamp(ns_str: &str) -> String {
    let ns: i64 = match ns_str.parse() {
        Ok(n) => n,
        Err(_) => return ns_str.to_string(),
    };
    let secs = ns / 1_000_000_000;
    let sub_ns = (ns % 1_000_000_000).unsigned_abs() as u32;
    chrono::DateTime::from_timestamp(secs, sub_ns)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| ns_str.to_string())
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

// Keep just enough of an unexpected response body to be useful, without
// dumping a large page into the caller's output.
fn truncate_body(body: &str) -> String {
    const MAX: usize = 500;
    if body.chars().count() > MAX {
        let head: String = body.chars().take(MAX).collect();
        format!("{head} (truncated)")
    } else {
        body.to_string()
    }
}

pub struct LokiClient {
    base_url: String,
    auth: crate::backend::Auth,
    service_label: String,
    level_label: Option<String>,
    default_limit: u32,
    max_limit: u32,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct LabelValuesResponse {
    #[serde(default)]
    data: Vec<String>,
}

#[derive(Deserialize)]
struct QueryRangeResponse {
    data: QueryRangeData,
}

#[derive(Deserialize)]
struct QueryRangeData {
    result: Vec<StreamResult>,
}

#[derive(Deserialize)]
struct StreamResult {
    values: Vec<(String, String)>,
}

impl LokiClient {
    pub fn new(
        base_url: &str,
        auth: crate::backend::Auth,
        service_label: &str,
        level_label: Option<&str>,
        default_limit: u32,
        max_limit: u32,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth,
            service_label: service_label.to_string(),
            level_label: level_label.map(String::from),
            default_limit,
            max_limit,
            http,
        }
    }

    async fn get_json<T, Q>(&self, url: &str, params: &Q) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
        Q: serde::Serialize + ?Sized,
    {
        let resp = self
            .auth
            .apply(self.http.get(url))
            .query(params)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    anyhow::anyhow!("cannot connect to Loki: {e}")
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
            bail!("Loki returned HTTP {s}: {}", truncate_body(&body));
        }

        Ok(resp.json().await?)
    }

    pub async fn list_services(&self) -> Result<Vec<String>> {
        let url = format!(
            "{}/loki/api/v1/label/{}/values",
            self.base_url, self.service_label
        );
        let now = Utc::now();
        let end = now.timestamp_nanos_opt().unwrap_or(0);
        let start = (now - chrono::Duration::days(30))
            .timestamp_nanos_opt()
            .unwrap_or(0);
        let parsed: LabelValuesResponse =
            self.get_json(&url, &[("start", start), ("end", end)]).await?;
        Ok(parsed.data)
    }

    pub async fn query_logs(&self, req: &LogRequest) -> Result<Vec<String>> {
        let query = LogQuery {
            service_label: self.service_label.clone(),
            service: req.service.clone(),
            start: req.start.clone(),
            end: req.end.clone(),
            limit: query::resolve_limit(req.limit, self.default_limit, self.max_limit),
            level: req.level.clone(),
            level_label: self.level_label.clone(),
            search: req.search.clone(),
            search_is_regex: req.search_is_regex,
        };
        let url = format!("{}/loki/api/v1/query_range", self.base_url);
        let parsed: QueryRangeResponse = self.get_json(&url, &query.to_params()?).await?;
        let lines: Vec<String> = parsed
            .data
            .result
            .into_iter()
            .flat_map(|stream| {
                stream
                    .values
                    .into_iter()
                    .map(|(ts_ns, line)| format!("{} | {}", format_ns_timestamp(&ts_ns), line))
            })
            .collect();
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn make_client(base_url: &str) -> LokiClient {
        LokiClient::new(
            base_url,
            crate::backend::Auth::Bearer("test-api-key".into()),
            "app",
            None,
            200,
            1000,
        )
    }

    #[tokio::test]
    async fn list_services_returns_data_array() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/loki/api/v1/label/app/values.*".into()),
            )
            .match_header("authorization", "Bearer test-api-key")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"success","data":["auth","api-gateway","db-proxy"]}"#)
            .create_async()
            .await;

        let client = make_client(&server.url());
        let services = client.list_services().await.unwrap();
        assert_eq!(services, vec!["auth", "api-gateway", "db-proxy"]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn query_logs_returns_log_lines() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/loki/api/v1/query_range.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "status": "success",
                "data": {
                    "resultType": "streams",
                    "result": [
                        {
                            "stream": {"app": "auth"},
                            "values": [
                                ["1686000000000000000", "user login failed"],
                                ["1686000060000000000", "invalid token"]
                            ]
                        }
                    ]
                }
            }"#,
            )
            .create_async()
            .await;

        let client = make_client(&server.url());
        let request = crate::loki::query::LogRequest {
            service: "auth".into(),
            start: "2026-06-12T13:00:00Z".into(),
            end: "2026-06-12T14:00:00Z".into(),
            level: None,
            search: None,
            search_is_regex: false,
            limit: Some(50),
        };
        let lines = client.query_logs(&request).await.unwrap();
        assert_eq!(
            lines,
            vec![
                "2023-06-05T21:20:00Z | user login failed",
                "2023-06-05T21:21:00Z | invalid token",
            ]
        );
        mock.assert_async().await;
    }

    #[test]
    fn format_ns_timestamp_formats_correctly() {
        // 1686000000000000000 ns = 2023-06-05T21:20:00Z
        assert_eq!(
            format_ns_timestamp("1686000000000000000"),
            "2023-06-05T21:20:00Z"
        );
    }

    #[tokio::test]
    async fn http_400_includes_response_body() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/loki/api/v1/query_range.*".into()),
            )
            .with_status(400)
            .with_body("query time range exceeds limit (5040h > 720h)")
            .create_async()
            .await;

        let client = make_client(&server.url());
        let request = crate::loki::query::LogRequest {
            service: "auth".into(),
            start: "2026-01-01T00:00:00Z".into(),
            end: "2026-06-14T00:00:00Z".into(),
            level: None,
            search: None,
            search_is_regex: false,
            limit: Some(50),
        };
        let err = client.query_logs(&request).await.unwrap_err().to_string();
        assert!(err.contains("400"), "got: {err}");
        assert!(err.contains("query time range exceeds limit"), "got: {err}");
    }

    #[tokio::test]
    async fn http_401_returns_friendly_message() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/loki/api/v1/label/app/values.*".into()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        let client = make_client(&server.url());
        let err = client.list_services().await.unwrap_err().to_string();
        assert!(err.contains("authentication failed"), "got: {err}");
    }
}
