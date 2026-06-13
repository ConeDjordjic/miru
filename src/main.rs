mod backend;
mod config;
mod loki;

fn main() {
    let config = config::Config::load().unwrap_or_else(|e| {
        eprintln!("miru: {e:#}");
        std::process::exit(1);
    });
    if let Err(e) = config.validate() {
        eprintln!("miru: {e}");
        std::process::exit(1);
    }
    eprintln!("miru: config loaded and validated");
}
