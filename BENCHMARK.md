# Local synthetic benchmark

Environment: local Python API (`server.py` + persistent `rrdtool -` child) and unified Rust server (`mrtg-rs`), identical temporary dataset with 9 host and 20 Docker RRDs, 90 five-minute updates and NaN values.

Parity comparison covered targets, uptime, docker CPU and an eight-target batch structure, including timestamps, keys, series and statistics. Numeric tolerance was 1e-6. All comparisons passed.

| Process / Component | Idle Memory | Loaded Memory |
|---|---:|---:|
| Python API (`server.py` `VmRSS`) | 24,516 KiB | 26,908 KiB |
| Persistent `rrdtool -` child (`VmRSS`) | 10,168 KiB | 10,168 KiB |
| `mrtg-web` (`nginx:alpine` container cgroup / 17 procs `VmRSS` sum) | 13,770 KiB (55,644 KiB sum) | 13,770 KiB (55,644 KiB sum) |
| **Old pair total (`mrtg-api` + `rrdtool -` + `mrtg-web` cgroup)** | **48,454 KiB (~47.3 MiB)** | **50,846 KiB (~49.7 MiB)** |
| **Rust `mrtg-rs` unified (single binary: API + `librrd` + static web)** | **13,200 KiB (~12.9 MiB)** | **15,184 KiB (~14.8 MiB)** |
Each latency row used 200 sequential HTTP requests, with unique end times for cold and a fixed end time for warm requests. Batch contained eight host targets. Values are milliseconds.

| Server | Endpoint | Cache | p50 | p95 |
|---|---|---|---:|---:|
| Python | query | cold | 0.805 | 0.882 |
| Python | query | warm | 0.615 | 0.683 |
| Python | query-batch | cold | 3.434 | 3.803 |
| Python | query-batch | warm | 1.804 | 1.935 |
| Rust | query | cold | 0.273 | 1.093 |
| Rust | query | warm | 0.201 | 0.288 |
| Rust | query-batch | cold | 0.653 | 0.790 |
| Rust | query-batch | warm | 0.358 | 0.416 |

Image `localhost/mrtg-rs:test`: 31,671,791 bytes according to `podman image inspect`. Static endpoints and configured host, footer and tab replacements worked in the local test. The healthcheck exited 0 with the server running and 1 on a closed port.

These are local measurements, not production sizing. The 055 container used a planned 64 MiB limit; the native collector service uses 96 MiB pending production measurement.

## Collector benchmark (2026-11-02)

Measured on a single Arch Linux host (reviewer re-run, production cadence) with temporary RRD directories, run as root so both collectors saw the same rootful and rootless Podman containers. Both read the live host `/proc`; each produced 69 RRDs.

- Python: one run of a temporary copy of the legacy `mrtg-collect.py` with only `DATA` changed. CPU from `wait4` rusage, which includes all waited children (`podman`, 69× `rrdtool`).
- Rust: the host's production config with only `data_dir` changed (`interval_secs = 300`, `sample_secs = 10`), loopback HTTP port. CPU is the change in `/proc/<pid>/stat` fields 14–17 (including waited `podman` children) over one full window between two boundaries: 30 gauge samples plus one boundary cycle.

| Per 5-minute window | Rust collector | Python collector |
|---|---:|---:|
| Boundary cycle wall time | 0.536 s (steady state; first cycle 1.570 s includes the 1 s CPU bootstrap sample) | 2.156 s |
| CPU, including children | **0.35 s** | **1.044 s** |
| Memory | 17,404 KiB RSS (whole `mrtg-rs`: collector + API + panel) | 55,152 KiB max RSS of the run |
| Process model | resident, no forks except `podman` discovery | Python start + `podman` + 69 `rrdtool` forks every 5 minutes |

Single-host measurement on live metrics, not a controlled fixture benchmark or a production sizing study.
