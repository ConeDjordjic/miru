mod backend;
mod config;
mod loki;
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
    let resolved = backend::resolve(&config.grafana).await.unwrap_or_else(|e| {
        eprintln!("miru: {e:#}");
        std::process::exit(1);
    });
    let loki = Arc::new(loki::LokiClient::new(
        &resolved.base_url,
        resolved.auth,
        &config.loki.service_label,
        config.loki.level_label.as_deref(),
        config.loki.default_limit,
        config.loki.max_limit,
    ));
    if let Err(e) = server::run(loki).await {
        eprintln!("miru: {e}");
        std::process::exit(1);
    }
}
