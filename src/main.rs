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
    let loki_backend = resolved.loki.expect("loki backend is required");
    let loki = Arc::new(loki::LokiClient::new(
        &loki_backend.base_url,
        loki_backend.auth,
        &config.loki.service_label,
        config.loki.level_label.as_deref(),
        config.loki.default_limit,
        config.loki.max_limit,
    ));
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
