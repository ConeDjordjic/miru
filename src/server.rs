use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::transport::stdio;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::loki::{LokiClient, query::LogQuery};

fn resolve_time_window(
    start: Option<String>,
    end: Option<String>,
    lookback_minutes: Option<u32>,
) -> (String, String) {
    let now = Utc::now();
    let fmt = |t: chrono::DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    match (start, end, lookback_minutes) {
        (Some(s), Some(e), _) => (s, e),
        (_, _, Some(mins)) => (fmt(now - chrono::Duration::minutes(mins as i64)), fmt(now)),
        // No explicit range given: default to the last 30 days. There is no
        // maximum window; pass start and end for anything longer.
        _ => (fmt(now - chrono::Duration::days(30)), fmt(now)),
    }
}

// Level names go into a LogQL query, so restrict them to word characters.
fn validate_level(level: &str) -> Result<(), String> {
    if level.is_empty()
        || !level
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "invalid level {level:?}: use a single level name like error, warn, crit, or trace"
        ));
    }
    Ok(())
}

#[derive(Deserialize, JsonSchema)]
struct QueryLogsParams {
    #[schemars(description = "Service name (from list_services)")]
    service: String,
    #[schemars(
        description = "How many minutes back to look from now (e.g. 30 for last 30 minutes). Preferred over start/end when you don't know the exact current time. Omit this and start/end to default to the last 30 days."
    )]
    lookback_minutes: Option<u32>,
    #[schemars(
        description = "Start time in ISO 8601 format. Only use if you know the exact current UTC time; otherwise use lookback_minutes."
    )]
    start: Option<String>,
    #[schemars(description = "End time in ISO 8601 format. Required if start is provided.")]
    end: Option<String>,
    #[schemars(
        description = "Maximum log lines to return (default: configured default, hard cap: configured max)"
    )]
    limit: Option<u32>,
    #[schemars(
        description = "Log level to filter by, such as error, warn, info, debug, crit, or trace"
    )]
    level: Option<String>,
    #[schemars(description = "Text to search for in log lines")]
    search: Option<String>,
    #[schemars(description = "When true, treat search as a regex pattern")]
    regex: Option<bool>,
}

#[derive(Clone)]
pub struct MiruServer {
    loki: Arc<LokiClient>,
}

#[tool_router]
impl MiruServer {
    #[tool(
        description = "List all service names available in Loki. Call this first to discover valid service names before querying logs."
    )]
    async fn list_services(&self) -> Result<String, String> {
        let services = self.loki.list_services().await.map_err(|e| e.to_string())?;
        serde_json::to_string(&services).map_err(|e| e.to_string())
    }

    #[tool(
        description = "Fetch log lines from a service. Results are newest first and capped by limit. Set the time range with lookback_minutes or with start and end; if you give none of them it defaults to the last 30 days. There is no maximum window, so pass start and end for anything longer. Optionally filter by level (such as error, warn, info, debug, crit, trace), search by text, or match a regex pattern. If Loki returns a parse error, retry without the level filter."
    )]
    async fn query_logs(
        &self,
        Parameters(p): Parameters<QueryLogsParams>,
    ) -> Result<String, String> {
        let (start, end) = resolve_time_window(p.start, p.end, p.lookback_minutes);
        if let Some(ref level) = p.level {
            validate_level(level)?;
        }
        let query = LogQuery {
            service_label: self.loki.service_label().to_string(),
            service: p.service,
            start,
            end,
            limit: p.limit.unwrap_or_else(|| self.loki.default_limit()),
            max_limit: self.loki.max_limit(),
            level: p.level,
            level_label: self.loki.level_label().map(String::from),
            search: p.search,
            search_is_regex: p.regex.unwrap_or(false),
        };
        let lines = self
            .loki
            .query_logs(&query)
            .await
            .map_err(|e| e.to_string())?;
        if lines.is_empty() {
            Ok("No logs found in this time window.".to_string())
        } else {
            Ok(lines.join("\n"))
        }
    }
}

#[tool_handler(
    name = "miru",
    version = "0.1.0",
    instructions = "Query Grafana Loki logs via miru. When describing what you're doing, say \"using miru\" not \"using Loki MCP tools\" or \"using the Loki API\". Use list_services first to discover services, then query_logs to fetch logs. Supports filtering by level (such as error, warn, info, debug, crit, trace) and searching by text or regex. Use lookback_minutes when the current time is not known precisely."
)]
impl ServerHandler for MiruServer {}

pub async fn run(loki: Arc<LokiClient>) -> Result<()> {
    MiruServer { loki }
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .waiting()
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_window_explicit_start_end() {
        let (s, e) = resolve_time_window(
            Some("2026-06-12T10:00:00Z".into()),
            Some("2026-06-12T11:00:00Z".into()),
            None,
        );
        assert_eq!(s, "2026-06-12T10:00:00Z");
        assert_eq!(e, "2026-06-12T11:00:00Z");
    }

    #[test]
    fn time_window_lookback_minutes_width() {
        let (s, e) = resolve_time_window(None, None, Some(30));
        let start = chrono::DateTime::parse_from_rfc3339(&s).unwrap();
        let end = chrono::DateTime::parse_from_rfc3339(&e).unwrap();
        assert_eq!((end - start).num_minutes(), 30);
    }

    #[test]
    fn time_window_defaults_to_30_days_when_unset() {
        let (s, e) = resolve_time_window(None, None, None);
        let start = chrono::DateTime::parse_from_rfc3339(&s).unwrap();
        let end = chrono::DateTime::parse_from_rfc3339(&e).unwrap();
        assert_eq!((end - start).num_days(), 30);
    }

    #[test]
    fn validate_level_accepts_common_names() {
        for lvl in [
            "error", "warn", "ERROR", "crit", "trace", "err", "level_5", "0",
        ] {
            assert!(validate_level(lvl).is_ok(), "rejected {lvl}");
        }
    }

    #[test]
    fn validate_level_rejects_injection_payload() {
        assert!(validate_level("error` |~ `secret").is_err());
        assert!(validate_level("a b").is_err());
        assert!(validate_level("").is_err());
    }

    #[tokio::test]
    async fn query_logs_returns_empty_message_when_no_results() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/loki/api/v1/query_range.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"success","data":{"resultType":"streams","result":[]}}"#)
            .create_async()
            .await;

        let loki = Arc::new(crate::loki::LokiClient::new(
            &server.url(),
            crate::backend::Auth::Bearer("test".into()),
            "app",
            None,
            200,
            1000,
        ));
        let miru = MiruServer { loki };
        let result = miru
            .query_logs(Parameters(QueryLogsParams {
                service: "auth".into(),
                lookback_minutes: Some(30),
                start: None,
                end: None,
                limit: None,
                level: None,
                search: None,
                regex: None,
            }))
            .await;
        assert_eq!(result, Ok("No logs found in this time window.".to_string()));
    }
}
