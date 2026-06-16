pub mod query;

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Deserialize;

use crate::backend::Endpoint;
use query::MetricRequest;

pub struct PrometheusClient {
    endpoint: Endpoint,
    target_points: u32,
    max_series: u32,
    min_step_seconds: u32,
}

#[derive(Deserialize)]
struct PromResponse {
    data: PromData,
}

#[derive(Deserialize)]
struct PromData {
    #[serde(rename = "resultType")]
    result_type: String,
    #[serde(default)]
    result: Vec<PromSeries>,
}

#[derive(Deserialize)]
struct MetadataResponse {
    #[serde(default)]
    data: BTreeMap<String, Vec<MetricMeta>>,
}

#[derive(Deserialize)]
struct MetricMeta {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    help: String,
}

#[derive(Deserialize)]
struct PromSeries {
    #[serde(default)]
    metric: BTreeMap<String, String>,
    #[serde(default)]
    values: Vec<(f64, String)>,
    #[serde(default)]
    value: Option<(f64, String)>,
}

fn n(x: f64) -> String {
    let r = (x * 1000.0).round() / 1000.0;
    format!("{r}")
}

fn format_unix_ts(ts: f64) -> String {
    let secs = ts.trunc() as i64;
    let sub_ns = (ts.fract().abs() * 1e9) as u32;
    chrono::DateTime::from_timestamp(secs, sub_ns)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn format_selector(metric: &BTreeMap<String, String>) -> String {
    let name = metric.get("__name__").cloned().unwrap_or_default();
    let labels: Vec<String> = metric
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__")
        .map(|(k, v)| format!("{k}=\"{v}\""))
        .collect();
    if labels.is_empty() {
        name
    } else {
        format!("{name}{{{}}}", labels.join(", "))
    }
}

fn summarize(values: &[(f64, String)]) -> Option<String> {
    let parsed: Vec<(f64, f64)> = values
        .iter()
        .filter_map(|(ts, v)| v.parse::<f64>().ok().map(|x| (*ts, x)))
        .collect();
    let (first_ts, first_v) = *parsed.first()?;
    let mut min = first_v;
    let mut max = first_v;
    let mut peak_ts = first_ts;
    let mut sum = 0.0;
    let mut last = first_v;
    for (ts, x) in &parsed {
        if *x < min {
            min = *x;
        }
        if *x > max {
            max = *x;
            peak_ts = *ts;
        }
        sum += *x;
        last = *x;
    }
    let avg = sum / parsed.len() as f64;
    Some(format!(
        "min={} avg={} max={}@{} last={}",
        n(min),
        n(avg),
        n(max),
        format_unix_ts(peak_ts),
        n(last)
    ))
}

fn format_metric(name: &str, meta: Option<&MetricMeta>) -> String {
    match meta {
        Some(m) if !m.help.is_empty() => format!("{name} ({}) {}", m.kind, m.help),
        Some(m) => format!("{name} ({})", m.kind),
        None => name.to_string(),
    }
}

fn over_cap_note(total: usize, cap: usize) -> String {
    format!(
        "... {} more series; aggregate in PromQL (e.g. sum by (label)) to narrow",
        total - cap
    )
}

fn shape_matrix(series: &[PromSeries], max_series: u32) -> Vec<String> {
    let cap = max_series as usize;
    let mut out = Vec::new();
    for s in series.iter().take(cap) {
        let mut block = format_selector(&s.metric);
        if let Some(sum) = summarize(&s.values) {
            block.push(' ');
            block.push_str(&sum);
        }
        for (ts, v) in &s.values {
            block.push_str(&format!("\n  {} {}", format_unix_ts(*ts), v));
        }
        out.push(block);
    }
    if series.len() > cap {
        out.push(over_cap_note(series.len(), cap));
    }
    out
}

fn shape_vector(series: &[PromSeries], max_series: u32) -> Vec<String> {
    let cap = max_series as usize;
    let mut out = Vec::new();
    for s in series.iter().take(cap) {
        let v = s.value.as_ref().map(|(_, v)| v.as_str()).unwrap_or("");
        out.push(format!("{} => {}", format_selector(&s.metric), v));
    }
    if series.len() > cap {
        out.push(over_cap_note(series.len(), cap));
    }
    out
}

impl PrometheusClient {
    pub fn new(
        base_url: &str,
        auth: crate::backend::Auth,
        target_points: u32,
        max_series: u32,
        min_step_seconds: u32,
    ) -> Self {
        Self {
            endpoint: Endpoint::new(base_url, auth, "Prometheus"),
            target_points,
            max_series,
            min_step_seconds,
        }
    }

    pub async fn list_metrics(&self, filter: Option<&str>) -> Result<Vec<String>> {
        let resp: MetadataResponse = self
            .endpoint
            .get_json("/api/v1/metadata", &Vec::<(&str, &str)>::new())
            .await?;
        let needle = filter.map(|f| f.to_lowercase());
        let out = resp
            .data
            .iter()
            .filter(|(name, _)| match &needle {
                Some(n) => name.to_lowercase().contains(n.as_str()),
                None => true,
            })
            .map(|(name, metas)| format_metric(name, metas.first()))
            .collect();
        Ok(out)
    }

    pub async fn query_metrics(&self, req: &MetricRequest) -> Result<Vec<String>> {
        let path = if req.instant {
            "/api/v1/query"
        } else {
            "/api/v1/query_range"
        };
        let params = req.to_params(self.target_points, self.min_step_seconds);
        let parsed: PromResponse = self.endpoint.get_json(path, &params).await?;
        let shaped = if parsed.data.result_type == "vector" {
            shape_vector(&parsed.data.result, self.max_series)
        } else {
            shape_matrix(&parsed.data.result, self.max_series)
        };
        Ok(shaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn matrix_series(name: &str, instance: &str, points: &[(f64, &str)]) -> PromSeries {
        let mut metric = BTreeMap::new();
        metric.insert("__name__".into(), name.into());
        metric.insert("instance".into(), instance.into());
        PromSeries {
            metric,
            values: points.iter().map(|(t, v)| (*t, v.to_string())).collect(),
            value: None,
        }
    }

    #[test]
    fn selector_includes_name_and_labels() {
        let mut metric = BTreeMap::new();
        metric.insert("__name__".into(), "cpu".into());
        metric.insert("instance".into(), "web-1".into());
        assert_eq!(format_selector(&metric), r#"cpu{instance="web-1"}"#);
    }

    #[test]
    fn selector_without_labels_is_just_the_name() {
        let mut metric = BTreeMap::new();
        metric.insert("__name__".into(), "cpu".into());
        assert_eq!(format_selector(&metric), "cpu");
    }

    #[test]
    fn matrix_block_has_summary_and_points() {
        let series = matrix_series(
            "cpu",
            "web-1",
            &[
                (1686000000.0, "12"),
                (1686000060.0, "88"),
                (1686000120.0, "20"),
            ],
        );
        let out = shape_matrix(&[series], 20);
        assert_eq!(out.len(), 1);
        let block = &out[0];
        assert!(block.contains(r#"cpu{instance="web-1"}"#), "got: {block}");
        assert!(block.contains("min=12"), "got: {block}");
        assert!(
            block.contains("max=88@2023-06-05T21:21:00Z"),
            "got: {block}"
        );
        assert!(block.contains("avg=40"), "got: {block}");
        assert!(block.contains("last=20"), "got: {block}");
        assert!(
            block.contains("\n  2023-06-05T21:20:00Z 12"),
            "got: {block}"
        );
    }

    #[test]
    fn matrix_caps_series_with_note() {
        let series: Vec<PromSeries> = (0..3)
            .map(|i| matrix_series("cpu", &format!("web-{i}"), &[(1686000000.0, "1")]))
            .collect();
        let out = shape_matrix(&series, 2);
        assert_eq!(out.len(), 3);
        assert!(out[2].contains("1 more series"), "got: {}", out[2]);
    }

    #[test]
    fn vector_renders_label_to_value() {
        let mut metric = BTreeMap::new();
        metric.insert("__name__".into(), "up".into());
        metric.insert("instance".into(), "web-1".into());
        let series = PromSeries {
            metric,
            values: vec![],
            value: Some((1686000000.0, "1".into())),
        };
        let out = shape_vector(&[series], 20);
        assert_eq!(out, vec![r#"up{instance="web-1"} => 1"#.to_string()]);
    }

    #[test]
    fn metric_with_help_is_enriched() {
        let meta = MetricMeta {
            kind: "counter".into(),
            help: "Total HTTP requests".into(),
        };
        assert_eq!(
            format_metric("http_requests_total", Some(&meta)),
            "http_requests_total (counter) Total HTTP requests"
        );
    }

    #[test]
    fn metric_without_help_shows_type_only() {
        let meta = MetricMeta {
            kind: "gauge".into(),
            help: String::new(),
        };
        assert_eq!(format_metric("in_flight", Some(&meta)), "in_flight (gauge)");
    }

    fn make_client(base_url: &str) -> PrometheusClient {
        PrometheusClient::new(
            base_url,
            crate::backend::Auth::Bearer("test".into()),
            100,
            20,
            15,
        )
    }

    #[tokio::test]
    async fn query_metrics_range_returns_shaped_matrix() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/api/v1/query_range.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"status":"success","data":{"resultType":"matrix","result":[
                    {"metric":{"__name__":"cpu","instance":"web-1"},
                     "values":[[1686000000,"12"],[1686000060,"88"]]}
                ]}}"#,
            )
            .create_async()
            .await;

        let client = make_client(&server.url());
        let req = MetricRequest {
            promql: "cpu".into(),
            start: "2026-06-12T13:00:00Z".into(),
            end: "2026-06-12T14:00:00Z".into(),
            step: None,
            instant: false,
        };
        let out = client.query_metrics(&req).await.unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("max=88"), "got: {}", out[0]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn list_metrics_filters_and_enriches() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r"/api/v1/metadata.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"status":"success","data":{
                    "http_requests_total":[{"type":"counter","help":"Total HTTP requests"}],
                    "node_cpu_seconds":[{"type":"counter","help":"CPU seconds"}]
                }}"#,
            )
            .create_async()
            .await;

        let client = make_client(&server.url());
        let out = client.list_metrics(Some("http")).await.unwrap();
        assert_eq!(
            out,
            vec!["http_requests_total (counter) Total HTTP requests".to_string()]
        );
    }

    #[tokio::test]
    async fn query_metrics_instant_returns_shaped_vector() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r"/api/v1/query.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"status":"success","data":{"resultType":"vector","result":[
                    {"metric":{"__name__":"up","instance":"web-1"},"value":[1686000000,"1"]}
                ]}}"#,
            )
            .create_async()
            .await;

        let client = make_client(&server.url());
        let req = MetricRequest {
            promql: "up".into(),
            start: "2026-06-12T13:00:00Z".into(),
            end: "2026-06-12T14:00:00Z".into(),
            step: None,
            instant: true,
        };
        let out = client.query_metrics(&req).await.unwrap();
        assert_eq!(out, vec![r#"up{instance="web-1"} => 1"#.to_string()]);
    }
}
