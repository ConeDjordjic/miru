mod backend;
mod config;
mod loki;
mod prometheus;
mod server;

use std::sync::Arc;

#[tokio::main]
async fn main() {
    let config = config::Config::load().unwrap_or_else(|e| {
        eprintln!("miru: {e:#}");
        std::process::exit(1);
    });
    if let Err(e) = config.validate() {
        eprintln!("miru: {e}");
        std::process::exit(1);
    }
    let resolved = backend::resolve(&config).await.unwrap_or_else(|e| {
        eprintln!("miru: {e:#}");
        std::process::exit(1);
    });
    let loki = match (resolved.loki, &config.loki) {
        (Some(backend), Some(lc)) => Some(Arc::new(loki::LokiClient::new(
            &backend.base_url,
            backend.auth,
            &lc.service_label,
            lc.level_label.as_deref(),
            lc.default_limit,
            lc.max_limit,
        ))),
        _ => None,
    };
    let prometheus = match (resolved.prometheus, &config.prometheus) {
        (Some(backend), Some(pc)) => Some(Arc::new(prometheus::PrometheusClient::new(
            &backend.base_url,
            backend.auth,
            pc.target_points,
            pc.max_series,
            pc.min_step_seconds,
        ))),
        _ => None,
    };
    if let Err(e) = server::run(loki, prometheus).await {
        eprintln!("miru: {e}");
        std::process::exit(1);
    }
}
