# mrtg-rs — agent rules

## Public repository

- This repo is public. Never commit real host names, internal paths, usernames or per-host config. `deploy/` holds only generic examples (`config.example.toml`, `mrtg-rs.example.env`); real per-host files live in the private infra repo.
- History was already rewritten once to remove host data (2026-11-04). Check `git diff --cached` for host names before every commit; a leak means another rewrite plus repo recreation.

## Invariants

- `www/index.html` contains placeholders replaced at startup in `AppState::new` (`src/lib.rs`): `localhost` (host), `Tailscale Protected` (footer) and the indented `Docker` tab label. Changing one side without the other silently breaks branding.
- Ship the binary and `www/` together. A host with an older `www/` (previous placeholder) keeps showing the old name under a new binary.
- The collector reads the host `/proc` and must run natively under systemd (`deploy/mrtg-rs.service`), not in a container. The container image is API + dashboard only.
- Never point `data_dir` at a classic MRTG (Perl) directory: its RRA layout differs.

## Container collection

- Only Podman is supported (`engine = "podman"` or `"none"`, validated in `src/config.rs`). Docker hosts use `engine = "none"` and get no container graphs; adding Docker means a new discovery path, not a config value.
- The `docker` names (`scope=docker`, `docker/docker_<name>_{cpu,mem}.rrd`, `BRAND_CONTAINER_TAB` default) are the legacy layout and also hold Podman data. Do not rename: existing RRDs and the dashboard depend on them.
- Discovery runs `podman ps` + `podman inspect` for root, then for each `rootless` entry via `runuser -u <user> -- env XDG_RUNTIME_DIR=/run/user/<uid>`. Requires running as root and `/run/user/<uid>` present (user lingering).
- Metrics come from cgroup v2 only (`/sys/fs/cgroup/<path>` from `/proc/<pid>/cgroup` `0::` line): CPU `cpu.stat usage_usec`, memory `memory.current - inactive_file`, limit `memory.max` (`max` = host `MemTotal`).
- Container names are sanitized (non `[A-Za-z0-9_]` becomes `_`) before `include`/`exclude` matching and RRD naming. Names that collide after sanitizing are skipped and counted as errors.

## Build and test

- `librrd.so.8` is linked dynamically (`build.rs`). Build release binaries inside `deploy/Containerfile.arch` or `deploy/Containerfile.debian` to match the target host glibc; do not copy a binary built on one distro to another.
- Alpine (`Dockerfile`) needs `-C target-feature=-crt-static` and `libgcc` at runtime; static musl linking fails on `librrd.a` glib symbols.
- `cargo test` needs `librrd` and the `rrdtool` CLI installed; tests compare native writes against `rrdtool`.
- As container PID 1 the binary ignores SIGTERM; run with `--init` or `podman stop` waits 10 s.

## Language

- Docs, comments and commit messages in English. Dashboard UI text and single-target API errors stay in pt-BR.
