# mrtg-rs

MRTG-style monitor in a single Rust binary: metrics collector, RRD storage through `librrd`, JSON API and a static web dashboard (ECharts).

Replaces the old `mrtg-collect.py` + `mrtg-api` (Python) + `mrtg-web` (nginx) stack. Measurements in [BENCHMARK.md](BENCHMARK.md).

## How it works

- **Collector** (optional): enabled when the TOML file at `MRTG_CONFIG` exists. Reads `/proc`, `/sys/class/hwmon`, `smartctl` and `podman`. Writes one RRD per target every `interval_secs`. Gauge metrics are sampled every `sample_secs` and aggregated at each cycle.
- **API**: reads RRDs from `data_dir` (TOML) or `DATA_DIR`. Responses are cached (LRU) for `CACHE_TTL` seconds.
- **Dashboard**: serves `www/` from `WWW_DIR`, substituting host name, footer and container tab label.

Without a config file the process runs API + dashboard only, over existing RRDs.

## Build

Requires stable Rust and `librrd` (`rrdtool` on Arch, `librrd-dev` on Debian, `rrdtool-dev` on Alpine).

```sh
cargo build --release --locked
cargo test
```

The binary links dynamically against `librrd.so.8`. To build a binary matching each host's glibc:

```sh
podman build -f deploy/Containerfile.arch -t mrtg-rs-build:arch .
podman build -f deploy/Containerfile.debian -t mrtg-rs-build:debian .
```

Copy `/src/target/release/mrtg-rs` out of the image into `dist/<distro>/`.

## Configuration

### Collector (`/etc/mrtg-rs/config.toml`)

```toml
[collector]
interval_secs = 300   # RRD step
sample_secs = 10      # 0 disables sampling between cycles
data_dir = "/var/lib/mrtg-rs"

[host]
metrics = ["cpu", "mem_swap", "loadavg", "uptime", "disk_usage", "network", "disk_io", "temperature", "smart"]
interfaces = ["eth0"]   # required by network
disks = ["sda"]         # required by disk_io and smart
disk_usage_path = "/"

[containers]
engine = "podman"       # "podman" or "none"
rootless = [{ user = "monitor", uid = 1000 }]
include = []           # empty = all containers; match sanitized names (my-app -> my_app)
exclude = []           # applied after include
```

Full example in `deploy/config.example.toml`.

### Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `MRTG_CONFIG` | `/etc/mrtg-rs/config.toml` | Collector config; missing = no collector |
| `LISTEN` | `$HOST:$PORT` | Comma-separated listen addresses |
| `HOST` / `PORT` | `0.0.0.0` / `8088` | Used when `LISTEN` is unset; `PORT` also used by `--healthcheck` |
| `DATA_DIR` | `/data` | RRD directory when the collector is off |
| `WWW_DIR` | `/app/www` | Dashboard files |
| `ALLOW_CIDRS` | empty (allow all) | Allowed networks; `/healthz` always accepts loopback |
| `CACHE_TTL` | `60` | Cache TTL in seconds |
| `CACHE_MAX_SIZE` / `CACHE_MAX_BYTES` | `500` / `16777216` | Cache limits |
| `MAX_TARGET_AGE` | `900` | Targets not updated for longer are hidden from `/targets` |
| `BRAND_HOST` / `BRAND_FOOTER` / `BRAND_CONTAINER_TAB` | `localhost` / `Tailscale Protected` / `Docker` | Dashboard labels |

## API

Every route also accepts the `/api` prefix.

| Route | Description |
|---|---|
| `GET /healthz` | `{"status":"ok"}` |
| `GET /targets[?all=1]` | Host and container targets; `all=1` includes inactive ones |
| `GET /query?scope=host\|docker&target=X&start=-1h[&end=now]` | Series for one target |
| `GET /query-batch?scope=...&target=A&target=B&start=...` | Up to 32 targets; returns `results` and `errors` |
| `GET /cache-stats` | Cache statistics |

`mrtg-rs --healthcheck` exits 0 when `/healthz` answers 200 on `127.0.0.1:$PORT`.

## Deployment

### systemd (collector + API + dashboard)

```sh
install -m755 dist/arch/mrtg-rs /usr/local/bin/mrtg-rs
install -Dm644 deploy/config.example.toml /etc/mrtg-rs/config.toml
install -Dm644 deploy/mrtg-rs.example.env /etc/mrtg-rs/mrtg-rs.env
install -Dm644 deploy/mrtg-rs.service /etc/systemd/system/mrtg-rs.service
systemctl daemon-reload && systemctl enable --now mrtg-rs
```

Runs as root: it needs the host `/proc`, `smartctl` and access to rootless users' `podman`. Copy `www/` to the `WWW_DIR` set in the env file.

### Container (API + dashboard only)

```sh
podman build -t mrtg-rs .
podman run -d -p 8088:8088 -v /var/lib/mrtg-rs:/data:ro mrtg-rs
```

Inside a container the collector would read the container's own metrics, so the collector runs natively under systemd.
