use std::{
    env,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[tokio::main]
async fn main() {
    let port = env::var("PORT").unwrap_or_else(|_| "8088".into());
    if env::args().any(|arg| arg == "--healthcheck") {
        let success = (|| -> Option<bool> {
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
            let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).ok()?;
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
            stream
                .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .ok()?;
            let mut response = String::new();
            stream.read_to_string(&mut response).ok()?;
            Some(response.starts_with("HTTP/1.1 200 ") || response.starts_with("HTTP/1.0 200 "))
        })()
        .unwrap_or(false);
        std::process::exit(if success { 0 } else { 1 });
    }
    if env::args().any(|arg| arg == "--help") {
        println!("mrtg-rs [--healthcheck]");
        return;
    }
    let config_path = env::var_os("MRTG_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/etc/mrtg-rs/config.toml".into());
    let config = mrtg_rs::config::Config::load(&config_path).unwrap_or_else(|e| {
        eprintln!("invalid collector config: {e}");
        std::process::exit(1)
    });
    let mut app_config = mrtg_rs::AppConfig::default();
    if let Some(config) = config {
        app_config.data_dir = config.collector.data_dir.clone();
        if !std::fs::read_to_string("/proc/1/comm")
            .is_ok_and(|s| ["systemd", "init"].contains(&s.trim()))
        {
            eprintln!(
                "warning: /proc/1 is not systemd or init; collector may read container metrics"
            );
        }
        let interval = config.collector.interval_secs;
        let sample = config.collector.sample_secs;
        let collector = Arc::new(Mutex::new(mrtg_rs::collector::Collector::new(
            config,
            "/proc".into(),
        )));
        tokio::spawn(async move {
            let tick = if sample == 0 { interval } else { sample };
            loop {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                let next = (now.as_secs() / tick + 1) * tick;
                tokio::time::sleep(Duration::from_secs(next) - now).await;
                let collector = collector.clone();
                let boundary = next.is_multiple_of(interval);
                tokio::task::spawn_blocking(move || {
                    let Ok(mut collector) = collector.try_lock() else {
                        eprintln!("collector: previous operation still running; skipping");
                        return;
                    };
                    if sample != 0 {
                        collector.sample();
                    }
                    if boundary {
                        let start = std::time::Instant::now();
                        let (updated, errors) = collector.cycle();
                        eprintln!(
                            "collector: duration_ms={} updated={updated} errors={errors}",
                            start.elapsed().as_millis()
                        );
                    }
                });
            }
        });
    }
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let listen = env::var("LISTEN").unwrap_or_else(|_| format!("{host}:{port}"));
    let app = mrtg_rs::router(mrtg_rs::AppState::new(app_config));
    let mut servers = Vec::new();
    for address in listen.split(',').map(str::trim) {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .expect("bind failed");
        let app = app.clone();
        servers.push(tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("server failed");
        }));
    }
    for server in servers {
        server.await.expect("server task failed");
    }
}
