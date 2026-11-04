pub mod cache;
pub mod collector;
pub mod config;
pub mod rrd;

use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, State},
    http::{header, Request, Response, StatusCode},
    routing::get,
    Router,
};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use ipnet::IpNet;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

#[derive(Clone)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub www_dir: PathBuf,
    pub max_target_age: u64,
    pub allow_cidrs: Vec<IpNet>,
    pub brand_host: String,
    pub brand_footer: String,
    pub brand_container_tab: String,
    pub cache_ttl: u64,
    pub cache_max_size: usize,
    pub cache_max_bytes: usize,
}
impl Default for AppConfig {
    fn default() -> Self {
        fn number<T: std::str::FromStr>(name: &str, default: T) -> T {
            env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        Self {
            data_dir: env::var("DATA_DIR")
                .unwrap_or_else(|_| "/data".into())
                .into(),
            www_dir: env::var("WWW_DIR")
                .unwrap_or_else(|_| "/app/www".into())
                .into(),
            max_target_age: number("MAX_TARGET_AGE", 900),
            allow_cidrs: env::var("ALLOW_CIDRS")
                .unwrap_or_default()
                .split(',')
                .filter(|v| !v.trim().is_empty())
                .map(|v| v.trim().parse().expect("invalid ALLOW_CIDRS entry"))
                .collect(),
            brand_host: env::var("BRAND_HOST").unwrap_or_else(|_| "localhost".into()),
            brand_footer: env::var("BRAND_FOOTER").unwrap_or_else(|_| "Tailscale Protected".into()),
            brand_container_tab: env::var("BRAND_CONTAINER_TAB")
                .unwrap_or_else(|_| "Docker".into()),
            cache_ttl: number("CACHE_TTL", 60),
            cache_max_size: number("CACHE_MAX_SIZE", 500),
            cache_max_bytes: number("CACHE_MAX_BYTES", 16777216),
        }
    }
}
#[derive(Clone)]
pub struct AppState {
    config: AppConfig,
    cache: Arc<Mutex<cache::ResponseCache>>,
    files: Arc<HashMap<&'static str, Vec<u8>>>,
}
impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let mut files = HashMap::new();
        for path in [
            "index.html",
            "app.js",
            "styles.css",
            "vendor/echarts.min.js",
        ] {
            if let Ok(mut bytes) = fs::read(config.www_dir.join(path)) {
                if path == "index.html" {
                    bytes = String::from_utf8_lossy(&bytes)
                        .replace("localhost", &config.brand_host)
                        .replace("Tailscale Protected", &config.brand_footer)
                        .replace(
                            "          Docker",
                            &format!("          {}", config.brand_container_tab),
                        )
                        .into_bytes();
                }
                files.insert(path, bytes);
            }
        }
        let cache = Arc::new(Mutex::new(cache::ResponseCache::new(
            config.cache_ttl,
            config.cache_max_size,
            config.cache_max_bytes,
        )));
        Self {
            config,
            cache,
            files: Arc::new(files),
        }
    }
}
pub fn router(state: AppState) -> Router {
    Router::new().fallback(get(handle)).with_state(state)
}
fn json_response(status: StatusCode, value: &impl Serialize) -> Response<Body> {
    bytes_response(
        status,
        serde_json::to_vec(value).unwrap().into(),
        "application/json; charset=utf-8",
        true,
    )
}
fn bytes_response(
    status: StatusCode,
    bytes: Bytes,
    content_type: &'static str,
    no_cache: bool,
) -> Response<Body> {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len());
    if no_cache {
        builder = builder.header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate");
    }
    builder.body(Body::from(bytes)).unwrap()
}
fn error(status: StatusCode, message: &str) -> Response<Body> {
    json_response(status, &json!({"error": message}))
}
fn valid_target(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn name(scope: &str, target: &str) -> String {
    if scope == "host" {
        if let Some(disk) = target.strip_prefix("temp_disco_") {
            return format!("Temperatura disco {disk}");
        }
        if let Some(chip) = target.strip_prefix("temp_") {
            return match chip {
                "coretemp" => "Temperatura CPU".into(),
                "acpitz" => "Temperatura ACPI".into(),
                _ => format!("Temperatura {chip}"),
            };
        }
        if let Some(disk) = target.strip_prefix("smart_") {
            return format!("SMART {disk}");
        }
        match target {
            "loadavg" => "Load Average",
            "cpu_user_idle" => "CPU User / Idle",
            "cpu_system_other" => "CPU System / Other",
            "cpu_iowait_steal" => "CPU I/O Wait / Steal",
            "mem_swap" => "Memória / Swap",
            "disk_io" => "Disk I/O",
            "disk_usage" => "Uso de Disco",
            "network" => "Tráfego de Rede",
            "uptime" => "Uptime",
            _ => target,
        }
        .into()
    } else if let Some(raw) = target.strip_prefix("docker_") {
        if let Some(n) = raw.strip_suffix("_cpu") {
            format!("{n} (CPU)")
        } else if let Some(n) = raw.strip_suffix("_mem") {
            format!("{n} (Memória)")
        } else {
            raw.into()
        }
    } else {
        target.into()
    }
}
#[derive(Serialize)]
struct Target {
    scope: &'static str,
    target: String,
    name: String,
}
fn list_targets(config: &AppConfig, all: bool) -> Vec<Target> {
    let mut result = Vec::new();
    for (scope, dir) in [
        ("host", config.data_dir.clone()),
        ("docker", config.data_dir.join("docker")),
    ] {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let filename = entry.file_name();
                let Some(filename) = filename.to_str() else {
                    continue;
                };
                let Some(target) = filename.strip_suffix(".rrd") else {
                    continue;
                };
                if !valid_target(target) {
                    continue;
                }
                if !all
                    && config.max_target_age > 0
                    && !entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| {
                        SystemTime::now()
                            .duration_since(t)
                            .unwrap_or(Duration::ZERO)
                            .as_secs()
                            <= config.max_target_age
                    })
                {
                    continue;
                }
                result.push(Target {
                    scope,
                    target: target.into(),
                    name: name(scope, target),
                });
            }
        }
    }
    result.sort_by_key(|v| (v.scope != "host", v.name.to_lowercase()));
    result
}
fn resolve_path(
    config: &AppConfig,
    scope: &str,
    target: &str,
) -> Result<PathBuf, (StatusCode, &'static str)> {
    if scope != "host" && scope != "docker" {
        return Err((StatusCode::BAD_REQUEST, "Invalid scope"));
    }
    if !valid_target(target) {
        return Err((StatusCode::BAD_REQUEST, "Invalid target identifier"));
    }
    let path = if scope == "host" {
        config.data_dir.join(format!("{target}.rrd"))
    } else {
        config.data_dir.join("docker").join(format!("{target}.rrd"))
    };
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err((StatusCode::BAD_REQUEST, "Symlinks not allowed"));
    }
    let base = config
        .data_dir
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found"))?;
    let path = path
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found"))?;
    if !path.starts_with(base) {
        return Err((StatusCode::BAD_REQUEST, "Path traversal attempt"));
    }
    if !path.is_file() || path.extension().is_none_or(|v| v != "rrd") {
        return Err((StatusCode::NOT_FOUND, "File not found"));
    }
    Ok(path)
}
fn normalize_time(value: &str, is_end: bool) -> Result<String, &'static str> {
    if value.trim().is_empty() {
        return Err("Parâmetro de tempo não pode ser vazio");
    }
    let value = value.trim();
    if is_end && value == "now" {
        return Ok(value.into());
    }
    if let Some(rel) = value.strip_prefix('-') {
        if rel.len() > 1
            && rel[..rel.len() - 1].bytes().all(|b| b.is_ascii_digit())
            && "smhdwMy".contains(&rel[rel.len() - 1..])
        {
            return Ok(value.into());
        }
    }
    if (9..=11).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(value.into());
    }
    let mut iso = value.to_owned();
    if iso.len() >= 16 {
        if let Some(i) = iso[10..].rfind(' ') {
            let i = i + 10;
            if i > 10
                && iso[i + 1..]
                    .bytes()
                    .next()
                    .is_some_and(|v| v.is_ascii_digit())
            {
                iso.replace_range(i..=i, "+");
            }
        }
    }
    let formats = [
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%d %H:%M%:z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%dT%H:%M%z",
        "%Y-%m-%d %H:%M:%S%z",
        "%Y-%m-%d %H:%M%z",
    ];
    for fmt in formats {
        if let Ok(dt) = DateTime::parse_from_str(&iso, fmt) {
            return Ok(dt.timestamp().to_string());
        }
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&iso, fmt) {
            return Local
                .from_local_datetime(&dt)
                .single()
                .map(|d| d.timestamp().to_string())
                .ok_or("Formato de data ISO inválido");
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(&iso, "%Y-%m-%d") {
        return Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .map(|d| d.timestamp().to_string())
            .ok_or("Formato de data ISO inválido");
    }
    if iso.len() >= 10
        && iso.as_bytes().get(4) == Some(&b'-')
        && iso.as_bytes().get(7) == Some(&b'-')
    {
        return Err("Formato de data ISO inválido");
    }
    Err("Formato de tempo inválido (use -15m, -24h, -7d, timestamp epoch ou ISO datetime)")
}
fn query(
    state: &AppState,
    scope: &str,
    target: &str,
    start: &str,
    end: &str,
) -> Result<Arc<[u8]>, (StatusCode, &'static str)> {
    let path = resolve_path(&state.config, scope, target)?;
    let key = format!("{}\0{start}\0{end}", path.display());
    if let Some(bytes) = state.cache.lock().unwrap().get(&key) {
        return Ok(bytes);
    }
    let raw = rrd::fetch_rrd(&path, start, end)
        .map_err(|_| (StatusCode::BAD_GATEWAY, "Fetch execution error"))?;
    let payload = rrd::process_series(target, raw.ds_names, raw.series, raw.timestamps, 2000);
    let bytes: Arc<[u8]> = serde_json::to_vec(&payload).unwrap().into();
    state.cache.lock().unwrap().put(key, Arc::clone(&bytes));
    Ok(bytes)
}
async fn handle(State(state): State<AppState>, request: Request<Body>) -> Response<Body> {
    let path = request.uri().path();
    if !state.config.allow_cidrs.is_empty() {
        let addr = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|v| v.0.ip().to_canonical());
        if !addr.is_some_and(|ip| {
            ((path == "/healthz" || path == "/api/healthz") && ip.is_loopback())
                || state.config.allow_cidrs.iter().any(|net| net.contains(&ip))
        }) {
            return error(StatusCode::FORBIDDEN, "Forbidden");
        }
    }
    let pairs: Vec<(String, String)> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .map(|(a, b)| (a.into_owned(), b.into_owned()))
            .collect();
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, v)| k == key && (key == "end" || !v.is_empty()))
            .map(|(_, v)| v.as_str())
    };
    match path {
        "/healthz" | "/api/healthz" => json_response(StatusCode::OK, &json!({"status":"ok"})),
        "/cache-stats" | "/api/cache-stats" => {
            json_response(StatusCode::OK, &state.cache.lock().unwrap().stats())
        }
        "/targets" | "/api/targets" => {
            let all = [get("all"), get("include_inactive")]
                .iter()
                .flatten()
                .any(|v| ["1", "true", "yes"].contains(v));
            json_response(StatusCode::OK, &list_targets(&state.config, all))
        }
        "/query" | "/api/query" | "/query-batch" | "/api/query-batch" => {
            let batch = path.ends_with("query-batch");
            let targets: Vec<&str> = pairs
                .iter()
                .filter(|(k, v)| k == "target" && !v.is_empty())
                .map(|(_, v)| v.as_str())
                .collect();
            let (Some(scope), Some(start)) = (get("scope"), get("start")) else {
                return error(
                    StatusCode::BAD_REQUEST,
                    if batch {
                        "Missing required query parameters: scope, target, start"
                    } else {
                        "Parâmetros obrigatórios ausentes (scope, target, start)"
                    },
                );
            };
            if targets.is_empty() {
                return error(
                    StatusCode::BAD_REQUEST,
                    if batch {
                        "Missing required query parameters: scope, target, start"
                    } else {
                        "Parâmetros obrigatórios ausentes (scope, target, start)"
                    },
                );
            }
            if scope != "host" && scope != "docker" {
                return error(
                    StatusCode::BAD_REQUEST,
                    if batch {
                        "Invalid scope parameter (must be host or docker)"
                    } else {
                        "Escopo inválido (use host ou docker)"
                    },
                );
            }
            if batch && targets.len() > 32 {
                return error(StatusCode::BAD_REQUEST, "Too many targets (max 32)");
            }
            if !batch && !valid_target(targets[0]) {
                return error(StatusCode::BAD_REQUEST, "Formato de target inválido");
            }
            let start = match normalize_time(start, false) {
                Ok(v) => v,
                Err(e) => return error(StatusCode::BAD_REQUEST, e),
            };
            let end = match normalize_time(get("end").unwrap_or("now"), true) {
                Ok(v) => v,
                Err(e) => return error(StatusCode::BAD_REQUEST, e),
            };
            if !batch {
                return match query(&state, scope, targets[0], &start, &end) {
                    Ok(bytes) => bytes_response(
                        StatusCode::OK,
                        Bytes::from_owner(bytes),
                        "application/json; charset=utf-8",
                        true,
                    ),
                    Err((code, e)) => error(code, e),
                };
            }
            let mut results: BTreeMap<String, Box<serde_json::value::RawValue>> = BTreeMap::new();
            let mut errors: BTreeMap<String, Value> = BTreeMap::new();
            for target in targets {
                if !valid_target(target) {
                    errors.insert(
                        target.into(),
                        json!({"status":400,"error":"Invalid target parameter"}),
                    );
                    continue;
                }
                match query(&state, scope, target, &start, &end) {
                    Ok(bytes) => {
                        results.insert(
                            target.into(),
                            serde_json::value::RawValue::from_string(
                                String::from_utf8(bytes.to_vec()).unwrap(),
                            )
                            .unwrap(),
                        );
                    }
                    Err((status, e)) => {
                        errors.insert(target.into(), json!({"status":status.as_u16(), "error":e}));
                    }
                }
            }
            json_response(
                StatusCode::OK,
                &json!({"results": results, "errors": errors}),
            )
        }
        _ => {
            let file = if path == "/" {
                "index.html"
            } else {
                path.trim_start_matches('/')
            };
            if [
                "index.html",
                "app.js",
                "styles.css",
                "vendor/echarts.min.js",
            ]
            .contains(&file)
            {
                if let Some(bytes) = state.files.get(file) {
                    let content_type = if file.ends_with(".html") {
                        "text/html; charset=utf-8"
                    } else if file.ends_with(".css") {
                        "text/css; charset=utf-8"
                    } else {
                        "application/javascript; charset=utf-8"
                    };
                    return bytes_response(
                        StatusCode::OK,
                        bytes.clone().into(),
                        content_type,
                        false,
                    );
                }
            }
            error(StatusCode::NOT_FOUND, "Endpoint not found")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::Request};
    use std::path::Path;
    use tower::ServiceExt;

    fn state(dir: &Path) -> AppState {
        AppState::new(AppConfig {
            data_dir: dir.into(),
            www_dir: dir.into(),
            max_target_age: 900,
            allow_cidrs: vec![],
            brand_host: "server1".into(),
            brand_footer: "LAN only".into(),
            brand_container_tab: "Podman".into(),
            cache_ttl: 60,
            cache_max_size: 10,
            cache_max_bytes: 100000,
        })
    }
    async fn request(state: AppState, uri: &str) -> (StatusCode, String) {
        let response = router(state)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }
    #[tokio::test]
    async fn collector_cycle_serves_targets_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, format!("[collector]\ninterval_secs=300\nsample_secs=10\ndata_dir='{}'\n[host]\nmetrics=['loadavg']\ndisk_usage_path='/'\n[containers]\nengine='none'\n", dir.path().display())).unwrap();
        let config = config::Config::load(&config_path).unwrap().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state(dir.path()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut collector = collector::Collector::new(config, "tests/fixtures/proc".into());
        collector.sample();
        assert_eq!(
            tokio::task::spawn_blocking(move || collector.cycle())
                .await
                .unwrap(),
            (1, 0)
        );
        let responses = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            [
                "/api/targets",
                "/api/query?scope=host&target=loadavg&start=-1h",
            ]
            .map(|uri| {
                let mut stream = std::net::TcpStream::connect(address).unwrap();
                write!(
                    stream,
                    "GET {uri} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                response
            })
        })
        .await
        .unwrap();
        assert!(responses[0].starts_with("HTTP/1.1 200 "));
        assert!(responses[0].contains("loadavg"));
        assert!(responses[1].starts_with("HTTP/1.1 200 "));
        server.abort();
    }
    #[test]
    fn target_names() {
        assert_eq!(name("host", "uptime"), "Uptime");
        assert_eq!(name("host", "temp_coretemp"), "Temperatura CPU");
        assert_eq!(name("host", "temp_acpitz"), "Temperatura ACPI");
        assert_eq!(name("host", "temp_k10temp"), "Temperatura k10temp");
        assert_eq!(name("host", "temp_disco_sda"), "Temperatura disco sda");
        assert_eq!(name("host", "smart_nvme0n1"), "SMART nvme0n1");
        assert_eq!(name("docker", "temp_coretemp"), "temp_coretemp");
        assert_eq!(name("docker", "docker_web_cpu"), "web (CPU)");
    }
    #[test]
    fn target_validation() {
        for s in ["../x", "a;b", "a b", "a/b", ""] {
            assert!(!valid_target(s));
        }
        assert!(valid_target("abc_123"));
    }
    #[test]
    fn relative_time() {
        assert_eq!(normalize_time("-24h", false).unwrap(), "-24h");
        assert!(normalize_time("-1q", false).is_err());
    }
    #[test]
    fn epoch_time() {
        assert_eq!(normalize_time("1696161600", false).unwrap(), "1696161600");
    }
    #[test]
    fn iso_time() {
        assert_eq!(
            normalize_time("2023-10-01T12:00:00+03:00", false).unwrap(),
            "1696150800"
        );
        assert_eq!(
            normalize_time("2023-10-01T12:00:00 03:00", false).unwrap(),
            "1696150800"
        );
    }
    #[test]
    fn invalid_time() {
        assert!(normalize_time("now", false).is_err());
        assert!(normalize_time("2023-99-99", false).is_err());
    }
    #[test]
    fn symlink_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        symlink("/etc/passwd", dir.path().join("bad.rrd")).unwrap();
        assert_eq!(
            resolve_path(&state(dir.path()).config, "host", "bad")
                .unwrap_err()
                .1,
            "Symlinks not allowed"
        );
    }
    #[test]
    fn target_listing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("uptime.rrd"), "").unwrap();
        fs::write(dir.path().join("bad-name.rrd"), "").unwrap();
        let result = list_targets(&state(dir.path()).config, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].target, "uptime");
    }
    #[tokio::test]
    async fn health_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            request(state(dir.path()), "/healthz").await.0,
            StatusCode::OK
        );
        assert_eq!(
            request(state(dir.path()), "/api/unknown").await.0,
            StatusCode::NOT_FOUND
        );
    }
    #[tokio::test]
    async fn cache_stats() {
        let dir = tempfile::tempdir().unwrap();
        let (_, body) = request(state(dir.path()), "/api/cache-stats").await;
        assert!(body.contains("max_bytes"));
    }
    #[tokio::test]
    async fn invalid_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (status, body) = request(state(dir.path()), "/query?scope=x&target=x&start=-1h").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("Escopo"));
    }
    #[tokio::test]
    async fn missing_target_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            request(state(dir.path()), "/query?scope=host&target=x&start=-1h")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    #[tokio::test]
    async fn corrupt_rrd() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.rrd"), "bad").unwrap();
        let (status, body) =
            request(state(dir.path()), "/query?scope=host&target=bad&start=-1h").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(!body.contains(dir.path().to_str().unwrap()));
    }
    #[tokio::test]
    async fn repeated_batch_targets() {
        let dir = tempfile::tempdir().unwrap();
        let (_, body) = request(
            state(dir.path()),
            "/query-batch?scope=host&target=a&target=b&start=-1h",
        )
        .await;
        assert!(body.contains("\"a\""));
        assert!(body.contains("\"b\""));
    }
    #[tokio::test]
    async fn batch_limit() {
        let dir = tempfile::tempdir().unwrap();
        let uri = format!(
            "/query-batch?scope=host&start=-1h{}",
            "&target=x".repeat(33)
        );
        assert_eq!(
            request(state(dir.path()), &uri).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    #[tokio::test]
    async fn branding() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("index.html"),
            "localhost Tailscale Protected           Docker",
        )
        .unwrap();
        let (_, body) = request(state(dir.path()), "/").await;
        assert!(body.contains("server1 LAN only           Podman"));
    }
    #[tokio::test]
    async fn empty_and_whitespace_parameters() {
        let dir = tempfile::tempdir().unwrap();
        for (uri, text) in [
            (
                "/query?scope=host&target=x&start=-1h&end=",
                "Parâmetro de tempo não pode ser vazio",
            ),
            (
                "/query?scope=host&target=x&start=%20",
                "Parâmetro de tempo não pode ser vazio",
            ),
            (
                "/query?scope=host&target=x&start=",
                "Parâmetros obrigatórios ausentes",
            ),
        ] {
            let (status, body) = request(state(dir.path()), uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body.contains(text), "{body}");
        }
    }

    #[tokio::test]
    async fn stale_targets_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("active.rrd"), "").unwrap();
        let stale = fs::File::create(dir.path().join("stale.rrd")).unwrap();
        stale
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        assert_eq!(list_targets(&state(dir.path()).config, false).len(), 1);
        for flag in ["all=1", "include_inactive=true"] {
            let (_, body) = request(state(dir.path()), &format!("/targets?{flag}")).await;
            let targets: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(targets.as_array().unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn static_content_types_and_branding() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("vendor")).unwrap();
        fs::write(
            dir.path().join("index.html"),
            "localhost Tailscale Protected           Docker",
        )
        .unwrap();
        for file in ["app.js", "styles.css", "vendor/echarts.min.js"] {
            fs::write(dir.path().join(file), "content").unwrap();
        }
        for (path, content_type) in [
            ("/", "text/html; charset=utf-8"),
            ("/app.js", "application/javascript; charset=utf-8"),
            ("/styles.css", "text/css; charset=utf-8"),
            (
                "/vendor/echarts.min.js",
                "application/javascript; charset=utf-8",
            ),
        ] {
            let response = router(state(dir.path()))
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
            assert!(response.headers().contains_key(header::CONTENT_LENGTH));
            if path == "/" {
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                assert_eq!(&body[..], b"server1 LAN only           Podman");
            }
        }
    }

    #[tokio::test]
    async fn allowed_and_denied_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path());
        state.config.allow_cidrs = vec!["10.10.10.0/24".parse().unwrap()];
        for (ip, path, expected) in [
            ("192.168.1.10:1234", "/targets", StatusCode::FORBIDDEN),
            ("10.10.10.25:1234", "/targets", StatusCode::OK),
            ("127.0.0.1:1234", "/healthz", StatusCode::OK),
        ] {
            let mut request = Request::builder().uri(path).body(Body::empty()).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
            assert_eq!(
                router(state.clone())
                    .oneshot(request)
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
    }
    #[tokio::test]
    async fn denied_ip() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path());
        state.config.allow_cidrs = vec!["10.10.10.0/24".parse().unwrap()];
        assert_eq!(request(state, "/targets").await.0, StatusCode::FORBIDDEN);
    }
}
