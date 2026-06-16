use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::transport::stdio;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::loki::{LokiClient, query::LogRequest};
use crate::prometheus::{PrometheusClient, query::MetricRequest};

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

#[derive(Deserialize, JsonSchema)]
struct QueryMetricsParams {
    #[schemars(
        description = "PromQL expression. Counters need a rate (e.g. rate(http_requests_total[5m])); gauges can be read directly. Discover metric names and their type with list_metrics first."
    )]
    promql: String,
    #[schemars(
        description = "How many minutes back to look from now. Preferred over start/end when you don't know the exact current time. Omit this and start/end to default to the last 30 days."
    )]
    lookback_minutes: Option<u32>,
    #[schemars(
        description = "Start time in ISO 8601 format. Only use if you know the exact current UTC time; otherwise use lookback_minutes."
    )]
    start: Option<String>,
    #[schemars(description = "End time in ISO 8601 format. Required if start is provided.")]
    end: Option<String>,
    #[schemars(
        description = "Resolution in seconds between data points. Omit to let miru pick a step that keeps the result readable."
    )]
    step: Option<u32>,
    #[schemars(
        description = "When true, evaluate at a single instant and return current values instead of a series over time."
    )]
    instant: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct ListMetricsParams {
    #[schemars(description = "Case-insensitive substring to filter metric names by")]
    filter: Option<String>,
}

#[derive(Clone)]
pub struct MiruServer {
    loki: Arc<LokiClient>,
    prometheus: Option<Arc<PrometheusClient>>,
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
        let request = LogRequest {
            service: p.service,
            start,
            end,
            level: p.level,
            search: p.search,
            search_is_regex: p.regex.unwrap_or(false),
            limit: p.limit,
        };
        let lines = self
            .loki
            .query_logs(&request)
            .await
            .map_err(|e| e.to_string())?;
        if lines.is_empty() {
            Ok("No logs found in this time window.".to_string())
        } else {
            Ok(lines.join("\n"))
        }
    }

    #[tool(
        description = "List metric names available in Prometheus, each with its type (counter, gauge, histogram) and help text. Call this first to discover metric names and whether a metric is a counter (use rate()) before writing PromQL. Optionally filter names by substring."
    )]
    async fn list_metrics(
        &self,
        Parameters(p): Parameters<ListMetricsParams>,
    ) -> Result<String, String> {
        let prometheus = self
            .prometheus
            .as_ref()
            .ok_or_else(|| "prometheus is not configured".to_string())?;
        let metrics = prometheus
            .list_metrics(p.filter.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        if metrics.is_empty() {
            Ok("No metrics found.".to_string())
        } else {
            Ok(metrics.join("\n"))
        }
    }

    #[tool(
        description = "Run a PromQL query against Prometheus to reason about metrics such as CPU, memory, or request rates. Returns a series over time by default (each series summarised with min/avg/max/peak then its data points); set instant=true for current values. Set the time range with lookback_minutes or start and end; if you give none it defaults to the last 30 days. Discover metric names with list_metrics first."
    )]
    async fn query_metrics(
        &self,
        Parameters(p): Parameters<QueryMetricsParams>,
    ) -> Result<String, String> {
        let prometheus = self
            .prometheus
            .as_ref()
            .ok_or_else(|| "prometheus is not configured".to_string())?;
        let (start, end) = resolve_time_window(p.start, p.end, p.lookback_minutes);
        let request = MetricRequest {
            promql: p.promql,
            start,
            end,
            step: p.step,
            instant: p.instant.unwrap_or(false),
        };
        let lines = prometheus
            .query_metrics(&request)
            .await
            .map_err(|e| e.to_string())?;
        if lines.is_empty() {
            Ok("No data for this query.".to_string())
        } else {
            Ok(lines.join("\n"))
        }
    }
}

#[tool_handler(
    name = "miru",
    version = "0.1.0",
    instructions = "Query Grafana Loki logs and Prometheus metrics via miru. When describing what you're doing, say \"using miru\" not \"using the Loki/Prometheus API\". For logs: use list_services first to discover services, then query_logs (filter by level such as error, warn, info, debug, crit, trace, and search by text or regex). For metrics: use list_metrics first to discover metric names and types, then query_metrics with PromQL (counters need rate(); set instant=true for current values). Use lookback_minutes when the current time is not known precisely. Metric tools return an error if Prometheus is not configured."
)]
impl ServerHandler for MiruServer {}

pub async fn run(loki: Arc<LokiClient>, prometheus: Option<Arc<PrometheusClient>>) -> Result<()> {
    MiruServer { loki, prometheus }
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
        let miru = MiruServer {
            loki,
            prometheus: None,
        };
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

    #[tokio::test]
    async fn invalid_level_returns_actionable_error_before_any_http() {
        let loki = Arc::new(crate::loki::LokiClient::new(
            "http://127.0.0.1:1",
            crate::backend::Auth::Bearer("test".into()),
            "app",
            None,
            200,
            1000,
        ));
        let miru = MiruServer {
            loki,
            prometheus: None,
        };
        let err = miru
            .query_logs(Parameters(QueryLogsParams {
                service: "auth".into(),
                lookback_minutes: Some(30),
                start: None,
                end: None,
                limit: None,
                level: Some("foo bar".into()),
                search: None,
                regex: None,
            }))
            .await
            .unwrap_err();
        assert!(err.contains("invalid level"), "got: {err}");
        assert!(err.contains("error, warn"), "got: {err}");
    }

    fn unreachable_loki() -> Arc<crate::loki::LokiClient> {
        Arc::new(crate::loki::LokiClient::new(
            "http://127.0.0.1:1",
            crate::backend::Auth::None,
            "app",
            None,
            200,
            1000,
        ))
    }

    #[tokio::test]
    async fn query_metrics_errors_when_prometheus_not_configured() {
        let miru = MiruServer {
            loki: unreachable_loki(),
            prometheus: None,
        };
        let err = miru
            .query_metrics(Parameters(QueryMetricsParams {
                promql: "up".into(),
                lookback_minutes: Some(30),
                start: None,
                end: None,
                step: None,
                instant: Some(true),
            }))
            .await
            .unwrap_err();
        assert!(err.contains("prometheus is not configured"), "got: {err}");
    }

    #[tokio::test]
    async fn query_metrics_returns_shaped_series() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/api/v1/query_range.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"status":"success","data":{"resultType":"matrix","result":[
                    {"metric":{"__name__":"cpu"},"values":[[1686000000,"5"],[1686000060,"50"]]}
                ]}}"#,
            )
            .create_async()
            .await;

        let prometheus = Some(Arc::new(crate::prometheus::PrometheusClient::new(
            &server.url(),
            crate::backend::Auth::None,
            100,
            20,
            15,
        )));
        let miru = MiruServer {
            loki: unreachable_loki(),
            prometheus,
        };
        let out = miru
            .query_metrics(Parameters(QueryMetricsParams {
                promql: "cpu".into(),
                lookback_minutes: Some(60),
                start: None,
                end: None,
                step: None,
                instant: Some(false),
            }))
            .await
            .unwrap();
        assert!(out.contains("max=50"), "got: {out}");
    }
}
