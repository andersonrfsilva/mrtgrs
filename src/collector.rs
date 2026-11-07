use crate::{config::Config, rrd::update_rrd};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fs,
    io::Read,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct GaugeAccumulator {
    sums: [f64; 2],
    count: u64,
}
impl GaugeAccumulator {
    pub fn add(&mut self, values: [f64; 2]) {
        self.sums[0] += values[0];
        self.sums[1] += values[1];
        self.count += 1;
    }
    pub fn take(&mut self) -> Option<[f64; 2]> {
        if self.count == 0 {
            return None;
        }
        let result = [
            self.sums[0] / self.count as f64,
            self.sums[1] / self.count as f64,
        ];
        *self = Self::default();
        Some(result)
    }
}

struct Container {
    name: String,
    pid: u32,
    cgroup: PathBuf,
    memory: GaugeAccumulator,
}
pub struct Collector {
    pub config: Config,
    pub proc_root: PathBuf,
    pub sys_root: PathBuf,
    cpu_previous: Option<[u64; 8]>,
    gauges: HashMap<String, GaugeAccumulator>,
    containers: Vec<Container>,
}
impl Collector {
    pub fn new(config: Config, proc_root: PathBuf) -> Self {
        Self {
            config,
            proc_root,
            sys_root: PathBuf::from("/sys"),
            cpu_previous: None,
            gauges: HashMap::new(),
            containers: Vec::new(),
        }
    }
    fn enabled(&self, name: &str) -> bool {
        self.config.host.metrics.iter().any(|m| m == name)
    }
    fn read(&self, file: &str) -> Result<String, String> {
        fs::read_to_string(self.proc_root.join(file)).map_err(|e| format!("{file}: {e}"))
    }
    fn memory(&self) -> Result<[f64; 2], String> {
        let text = self.read("meminfo")?;
        let map: HashMap<_, _> = text
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                Some((
                    key,
                    value.split_whitespace().next()?.parse::<f64>().ok()? * 1024.0,
                ))
            })
            .collect();
        let get = |key| {
            map.get(key)
                .copied()
                .ok_or_else(|| format!("missing meminfo {key}"))
        };
        Ok([
            get("MemTotal")? - get("MemAvailable")?,
            get("SwapTotal")? - get("SwapFree")?,
        ])
    }
    fn load(&self) -> Result<[f64; 2], String> {
        let text = self.read("loadavg")?;
        let mut fields = text.split_whitespace();
        let mut next = || {
            fields
                .next()
                .ok_or_else(|| "short loadavg".to_string())?
                .parse::<f64>()
                .map_err(|e| e.to_string())
        };
        Ok([next()?, next()?])
    }
    fn cpu(&self) -> Result<[u64; 8], String> {
        let text = self.read("stat")?;
        let line = text.lines().next().ok_or("empty stat")?;
        let values: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .take(8)
            .map(|v| v.parse::<u64>().map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        values.try_into().map_err(|_| "short cpu stat".into())
    }
    fn network(&self) -> Result<[u64; 2], String> {
        let text = self.read("net/dev")?;
        let mut result = [0, 0];
        for line in text.lines().skip(2) {
            if let Some((name, values)) = line.split_once(':') {
                if self.config.host.interfaces.iter().any(|i| i == name.trim()) {
                    let fields: Vec<_> = values.split_whitespace().collect();
                    if fields.len() < 9 {
                        return Err("short net/dev".into());
                    }
                    result[0] += fields[0].parse::<u64>().map_err(|e| e.to_string())?;
                    result[1] += fields[8].parse::<u64>().map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(result)
    }
    fn disk_io(&self) -> Result<[u64; 2], String> {
        let text = self.read("diskstats")?;
        let mut result = [0, 0];
        for line in text.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() >= 10 && self.config.host.disks.iter().any(|d| d == fields[2]) {
                result[0] += fields[5].parse::<u64>().map_err(|e| e.to_string())? * 512;
                result[1] += fields[9].parse::<u64>().map_err(|e| e.to_string())? * 512;
            }
        }
        Ok(result)
    }
    fn disk_usage(&self) -> Result<[u64; 2], String> {
        let path = CString::new(self.config.host.disk_usage_path.as_os_str().as_bytes())
            .map_err(|e| e.to_string())?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let stat = unsafe { stat.assume_init() };
        Ok([
            (stat.f_blocks - stat.f_bfree) * stat.f_frsize,
            stat.f_bavail * stat.f_frsize,
        ])
    }
    pub fn sample(&mut self) {
        for name in ["mem_swap", "loadavg"] {
            if self.enabled(name) {
                let value = if name == "mem_swap" {
                    self.memory()
                } else {
                    self.load()
                };
                match value {
                    Ok(v) => self.gauges.entry(name.into()).or_default().add(v),
                    Err(e) => eprintln!("{name}: {e}"),
                }
            }
        }
        if self.enabled("temperature") {
            for (name, value) in self.temperatures() {
                self.gauges.entry(name).or_default().add(value);
            }
        }
        self.containers
            .retain_mut(|container| match container_memory(&container.cgroup) {
                Ok(value) => {
                    container.memory.add([value as f64, 0.0]);
                    true
                }
                Err(e) => {
                    eprintln!("{}: {e}", container.name);
                    false
                }
            });
    }
    fn write(
        &self,
        name: &str,
        values: [String; 2],
        kinds: &[&str],
        errors: &mut usize,
        updated: &mut usize,
    ) {
        match update_rrd(
            &self.config.collector.data_dir.join(format!("{name}.rrd")),
            &values,
            kinds,
        ) {
            Ok(()) => *updated += 1,
            Err(e) => {
                *errors += 1;
                eprintln!("{name}: {e}");
            }
        }
    }
    fn discover(&self) -> (Vec<Container>, usize) {
        if self.config.containers.engine == "none" {
            return (Vec::new(), 0);
        }
        let mut items = Vec::new();
        let mut errors = 0;
        for user in std::iter::once(None).chain(self.config.containers.rootless.iter().map(Some)) {
            let mut prefix = Vec::new();
            if let Some(user) = user {
                prefix = vec![
                    "runuser".to_string(),
                    "-u".into(),
                    user.user.clone(),
                    "--".into(),
                    "env".into(),
                    format!("XDG_RUNTIME_DIR=/run/user/{}", user.uid),
                ];
            }
            let engine = &self.config.containers.engine;
            let mut cmd = prefix.clone();
            cmd.extend([engine.clone(), "ps".into(), "-q".into()]);
            let ids = match command(&cmd) {
                Ok(s) => s,
                Err(e) => {
                    errors += 1;
                    eprintln!("{engine} ps: {e}");
                    continue;
                }
            };
            let ids: Vec<_> = ids.split_whitespace().collect();
            if ids.is_empty() {
                continue;
            }
            let mut cmd = prefix;
            cmd.extend([engine.clone(), "inspect".into()]);
            cmd.extend(ids.into_iter().map(str::to_owned));
            match command(&cmd).and_then(|s| {
                serde_json::from_str::<Vec<serde_json::Value>>(&s).map_err(|e| e.to_string())
            }) {
                Ok(v) => items.extend(v),
                Err(e) => {
                    errors += 1;
                    eprintln!("{engine} inspect: {e}");
                }
            }
        }
        self.containers_from_items(items, errors)
    }
    fn containers_from_items(
        &self,
        items: Vec<serde_json::Value>,
        mut errors: usize,
    ) -> (Vec<Container>, usize) {
        let mut seen = HashSet::new();
        let mut containers = Vec::new();
        for item in items {
            // Swarm task names (`svc.1.<task id>`) change on every redeploy; the service name is stable.
            let Some(raw) = item
                .pointer("/Config/Labels/com.docker.swarm.service.name")
                .or_else(|| item.get("Name"))
                .and_then(|v| v.as_str())
            else {
                errors += 1;
                continue;
            };
            let name: String = raw
                .trim_start_matches('/')
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if !self.config.containers.include.is_empty()
                && !self.config.containers.include.contains(&name)
                || self.config.containers.exclude.contains(&name)
            {
                continue;
            }
            if !seen.insert(name.clone()) {
                errors += 1;
                eprintln!("container name collision: {name}");
                continue;
            }
            let Some(pid) = item
                .pointer("/State/Pid")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
            else {
                errors += 1;
                continue;
            };
            let path = self.proc_root.join(pid.to_string()).join("cgroup");
            let cgroup = fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    text.lines()
                        .find_map(|l| l.strip_prefix("0::"))
                        .map(|p| Path::new("/sys/fs/cgroup").join(p.trim_start_matches('/')))
                        .ok_or_else(|| "missing cgroup v2".into())
                });
            match cgroup {
                Ok(cgroup) => containers.push(Container {
                    name,
                    pid,
                    cgroup,
                    memory: GaugeAccumulator::default(),
                }),
                Err(e) if !self.proc_root.join(pid.to_string()).exists() => {
                    let _ = e;
                }
                Err(e) => {
                    errors += 1;
                    eprintln!("{name}: {e}");
                }
            }
        }
        (containers, errors)
    }
    fn collect_containers(&mut self, errors: &mut usize, updated: &mut usize) {
        let (mut discovered, count) = self.discover();
        *errors += count;
        for container in &mut discovered {
            if let Some(old) = self.containers.iter_mut().find(|c| {
                c.name == container.name && c.pid == container.pid && c.cgroup == container.cgroup
            }) {
                std::mem::swap(&mut old.memory, &mut container.memory);
            }
            let result = (|| -> Result<_, String> {
                let cpu = keyed_value(&container.cgroup.join("cpu.stat"), "usage_usec")? / 10000;
                let used = match container.memory.take() {
                    Some(v) => v[0],
                    None => container_memory(&container.cgroup)? as f64,
                };
                let total = self
                    .read("meminfo")?
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("MemTotal:")
                            .and_then(|s| s.split_whitespace().next())
                            .and_then(|n| n.parse::<u64>().ok())
                    })
                    .ok_or("missing MemTotal")?
                    * 1024;
                let limit = memory_limit(&container.cgroup, total)?;
                Ok((cpu, used, limit))
            })();
            match result {
                Ok((cpu, used, limit)) => {
                    self.write(
                        &format!("docker/docker_{}_cpu", container.name),
                        [cpu.to_string(), "0".into()],
                        &["DERIVE", "GAUGE"],
                        errors,
                        updated,
                    );
                    self.write(
                        &format!("docker/docker_{}_mem", container.name),
                        [used.to_string(), limit.to_string()],
                        &["GAUGE", "GAUGE"],
                        errors,
                        updated,
                    );
                }
                Err(e) if !self.proc_root.join(container.pid.to_string()).exists() => {
                    let _ = e;
                }
                Err(e) => {
                    *errors += 1;
                    eprintln!("{}: {e}", container.name);
                }
            }
        }
        self.containers = discovered;
    }
    pub fn cycle(&mut self) -> (usize, usize) {
        let (mut updated, mut errors) = (0, 0);
        for name in ["mem_swap", "loadavg"] {
            if self.enabled(name) {
                let value = self
                    .gauges
                    .entry(name.into())
                    .or_default()
                    .take()
                    .map(Ok)
                    .unwrap_or_else(|| {
                        if name == "mem_swap" {
                            self.memory()
                        } else {
                            self.load()
                        }
                    });
                match value {
                    Ok(v) => self.write(
                        name,
                        [v[0].to_string(), v[1].to_string()],
                        &["GAUGE", "GAUGE"],
                        &mut errors,
                        &mut updated,
                    ),
                    Err(e) => {
                        errors += 1;
                        eprintln!("{name}: {e}");
                    }
                }
            }
        }
        if self.enabled("temperature") {
            let names: Vec<_> = self
                .gauges
                .keys()
                .filter(|name| name.starts_with("temp_"))
                .cloned()
                .collect();
            if names.is_empty() {
                for (name, value) in self.temperatures() {
                    self.write(
                        &name,
                        value.map(|v| v.to_string()),
                        &["GAUGE", "GAUGE"],
                        &mut errors,
                        &mut updated,
                    );
                }
            } else {
                for name in names {
                    if let Some(value) =
                        self.gauges.remove(&name).and_then(|mut gauge| gauge.take())
                    {
                        self.write(
                            &name,
                            value.map(|v| v.to_string()),
                            &["GAUGE", "GAUGE"],
                            &mut errors,
                            &mut updated,
                        );
                    }
                }
            }
        }
        if self.enabled("smart") {
            self.collect_smart(&mut errors, &mut updated);
        }
        if self.enabled("cpu") {
            self.write_cpu(&mut errors, &mut updated);
        }
        for name in ["uptime", "disk_usage", "network", "disk_io"] {
            if self.enabled(name) {
                let value: Result<[String; 2], String> = match name {
                    "uptime" => self.read("uptime").and_then(|s| {
                        s.split_whitespace()
                            .next()
                            .map(|v| [v.to_string(), "0".into()])
                            .ok_or_else(|| "empty uptime".into())
                    }),
                    "disk_usage" => self.disk_usage().map(|v| v.map(|n| n.to_string())),
                    "network" => self.network().map(|v| v.map(|n| n.to_string())),
                    _ => self.disk_io().map(|v| v.map(|n| n.to_string())),
                };
                match value {
                    Ok(v) => self.write(
                        name,
                        v,
                        if name == "network" || name == "disk_io" {
                            &["DERIVE", "DERIVE"]
                        } else {
                            &["GAUGE", "GAUGE"]
                        },
                        &mut errors,
                        &mut updated,
                    ),
                    Err(e) => {
                        errors += 1;
                        eprintln!("{name}: {e}");
                    }
                }
            }
        }
        self.collect_containers(&mut errors, &mut updated);
        (updated, errors)
    }
    fn temperatures(&self) -> Vec<(String, [f64; 2])> {
        let root = self.sys_root.join("class/hwmon");
        let mut entries: Vec<_> = match fs::read_dir(&root) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect(),
            Err(e) => {
                eprintln!("temperature {}: {e}", root.display());
                return Vec::new();
            }
        };
        entries.sort();
        let mut counts = HashMap::<String, usize>::new();
        let mut result = Vec::new();
        for path in entries {
            let read_chip = || -> Result<String, String> {
                fs::read_to_string(path.join("name")).map_err(|e| e.to_string())
            };
            let chip = match read_chip() {
                Ok(chip) => chip.trim().to_string(),
                Err(e) => {
                    eprintln!("temperature {}: {e}", path.display());
                    continue;
                }
            };
            if chip == "nvme" || chip == "drivetemp" {
                continue;
            }
            let chip: String = chip
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let values = (|| -> Result<Vec<(String, f64)>, String> {
                let mut values = Vec::new();
                for entry in fs::read_dir(&path).map_err(|e| e.to_string())? {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let filename = entry.file_name();
                    let filename = filename.to_string_lossy();
                    if let Some(number) = filename
                        .strip_prefix("temp")
                        .and_then(|s| s.strip_suffix("_input"))
                    {
                        if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
                            let text =
                                fs::read_to_string(entry.path()).map_err(|e| e.to_string())?;
                            values.push((
                                number.to_string(),
                                text.trim().parse::<f64>().map_err(|e| e.to_string())? / 1000.0,
                            ));
                        }
                    }
                }
                Ok(values)
            })();
            let values = match values {
                // Chip sem sensor de temperatura (ex.: hp-wmi): não é erro.
                Ok(values) if values.is_empty() => continue,
                Ok(values) => values,
                Err(e) => {
                    eprintln!("temp_{chip}: {e}");
                    continue;
                }
            };
            let count = counts.entry(chip.clone()).or_default();
            *count += 1;
            let chip = if *count == 1 {
                format!("temp_{chip}")
            } else {
                format!("temp_{chip}_{count}")
            };
            // One RRD per tempN_input; ds1 is unused, like temp_disco_*.
            for (number, value) in values {
                result.push((format!("{chip}_s{number}"), [value, 0.0]));
            }
        }
        result
    }
    fn collect_smart(&self, errors: &mut usize, updated: &mut usize) {
        for disk in &self.config.host.disks {
            let args = vec![
                "smartctl".into(),
                "--json".into(),
                "-H".into(),
                "-A".into(),
                "-n".into(),
                "standby".into(),
                format!("/dev/{disk}"),
            ];
            let output = match command_with_timeout_output(&args, Duration::from_secs(30), true) {
                Ok(output) => output,
                Err(e) => {
                    *errors += 1;
                    eprintln!("smart {disk}: {e}");
                    if e.starts_with("spawn: ") {
                        break;
                    }
                    continue;
                }
            };
            let data: serde_json::Value = match serde_json::from_str(&output) {
                Ok(data) => data,
                Err(e) => {
                    *errors += 1;
                    eprintln!("smart {disk}: {e}");
                    continue;
                }
            };
            if data
                .pointer("/power_mode")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v.eq_ignore_ascii_case("standby"))
                || data
                    .pointer("/smartctl/messages")
                    .and_then(|v| v.as_array())
                    .is_some_and(|messages| {
                        messages.iter().any(|m| {
                            m.get("string")
                                .and_then(|v| v.as_str())
                                .is_some_and(|s| s.to_ascii_lowercase().contains("standby"))
                        })
                    })
            {
                continue;
            }
            if data
                .pointer("/smart_status/passed")
                .and_then(|v| v.as_bool())
                == Some(false)
            {
                *errors += 1;
                eprintln!("smart {disk}: SMART health FAILED");
            }
            if let Some(temp) = data
                .pointer("/temperature/current")
                .and_then(|v| v.as_f64())
            {
                self.write(
                    &format!("temp_disco_{disk}"),
                    [temp.to_string(), "0".into()],
                    &["GAUGE", "GAUGE"],
                    errors,
                    updated,
                );
            }
            // Atributo ausente vira "U" (desconhecido) no RRD, nunca 0: zero diria "saudável" sem medir.
            let values: Option<[Option<u64>; 2]> =
                if let Some(nvme) = data.get("nvme_smart_health_information_log") {
                    Some([
                        nvme.get("media_errors").and_then(|v| v.as_u64()),
                        nvme.get("critical_warning").and_then(|v| v.as_u64()),
                    ])
                } else {
                    data.pointer("/ata_smart_attributes/table")
                        .and_then(|v| v.as_array())
                        .map(|table| {
                            let raw = |id| {
                                table
                                    .iter()
                                    .find(|v| v.get("id").and_then(|v| v.as_u64()) == Some(id))
                                    .and_then(|v| v.pointer("/raw/value"))
                                    .and_then(|v| v.as_u64())
                            };
                            let pending = [raw(197), raw(198)];
                            let pending = if pending.iter().all(Option::is_none) {
                                None
                            } else {
                                Some(pending.iter().flatten().sum())
                            };
                            [raw(5), pending]
                        })
                };
            if let Some(values) = values.filter(|v| v.iter().any(Option::is_some)) {
                self.write(
                    &format!("smart_{disk}"),
                    values.map(|v| v.map_or_else(|| "U".to_string(), |v| v.to_string())),
                    &["GAUGE", "GAUGE"],
                    errors,
                    updated,
                );
            }
        }
    }
    fn write_cpu(&mut self, errors: &mut usize, updated: &mut usize) {
        let result = (|| -> Result<_, String> {
            let mut current = self.cpu()?;
            let previous = match self.cpu_previous {
                Some(previous) => previous,
                None => {
                    let initial = current;
                    std::thread::sleep(Duration::from_secs(1));
                    current = self.cpu()?;
                    initial
                }
            };
            let delta: [u64; 8] = std::array::from_fn(|i| current[i].saturating_sub(previous[i]));
            let total: u64 = delta.iter().sum();
            if total != 0 {
                let pct = |n: u64| (n as f64 * 100.0 / total as f64).to_string();
                for (name, a, b) in [
                    ("cpu_user_idle", delta[0] + delta[1], delta[3]),
                    ("cpu_system_other", delta[2], delta[5] + delta[6]),
                    ("cpu_iowait_steal", delta[4], delta[7]),
                ] {
                    self.write(name, [pct(a), pct(b)], &["GAUGE", "GAUGE"], errors, updated);
                }
            }
            self.cpu_previous = Some(current);
            Ok(())
        })();
        if let Err(e) = result {
            *errors += 1;
            eprintln!("cpu: {e}");
        }
    }
}

fn keyed_value(path: &Path, key: &str) -> Result<u64, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == key)
                .then(|| fields.next()?.parse::<u64>().ok())
                .flatten()
        })
        .ok_or_else(|| format!("{}: missing {key}", path.display()))
}
fn container_memory(path: &Path) -> Result<i64, String> {
    let current = fs::read_to_string(path.join("memory.current"))
        .map_err(|e| e.to_string())?
        .trim()
        .parse::<i64>()
        .map_err(|e| e.to_string())?;
    Ok(current - keyed_value(&path.join("memory.stat"), "inactive_file")? as i64)
}
fn memory_limit(path: &Path, total: u64) -> Result<u64, String> {
    let value = fs::read_to_string(path.join("memory.max")).map_err(|e| e.to_string())?;
    if value.trim() == "max" {
        Ok(total)
    } else {
        value.trim().parse::<u64>().map_err(|e| e.to_string())
    }
}
fn command(args: &[String]) -> Result<String, String> {
    command_with_timeout(args, Duration::from_secs(30))
}
fn command_with_timeout(args: &[String], timeout: Duration) -> Result<String, String> {
    command_with_timeout_output(args, timeout, false)
}
fn command_with_timeout_output(
    args: &[String],
    timeout: Duration,
    accept_failure: bool,
) -> Result<String, String> {
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("missing command stdout")?;
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        stdout.read_to_string(&mut output).map(|_| output)
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = reader
                    .join()
                    .map_err(|_| "command reader failed")?
                    .map_err(|e| e.to_string())?;
                return if status.success() || accept_failure {
                    Ok(output)
                } else {
                    Err(format!("command failed: {status}"))
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("command timed out".into());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collector_cgroup_values() {
        let path = Path::new("tests/fixtures/cgroup");
        assert_eq!(container_memory(path).unwrap(), 8000);
        assert_eq!(
            keyed_value(&path.join("cpu.stat"), "usage_usec").unwrap() / 10000,
            123
        );
    }
    #[test]
    fn collector_container_discovery_filters_and_collisions() {
        let dir = tempfile::tempdir().unwrap();
        for pid in [101, 102, 103] {
            let p = dir.path().join(pid.to_string());
            fs::create_dir(&p).unwrap();
            fs::write(p.join("cgroup"), format!("0::/test/{pid}\n")).unwrap();
        }
        let mut c = collector(dir.path());
        c.proc_root = dir.path().into();
        let items: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/fixtures/inspect.json")).unwrap();
        let (found, errors) = c.containers_from_items(items.clone(), 0);
        assert_eq!(
            found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["web_app", "database", "api_svc"]
        );
        assert_eq!(errors, 1);
        c.config.containers.include = vec!["database".into(), "web_app".into()];
        c.config.containers.exclude = vec!["web_app".into()];
        let (found, errors) = c.containers_from_items(items, 0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "database");
        assert_eq!(errors, 0);
    }
    #[test]
    fn collector_container_limits_sampling_and_timeout() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("memory.current"), "10000").unwrap();
        fs::write(dir.path().join("memory.stat"), "inactive_file 2000").unwrap();
        fs::write(dir.path().join("memory.max"), "max").unwrap();
        assert_eq!(memory_limit(dir.path(), 50000).unwrap(), 50000);
        fs::write(dir.path().join("memory.max"), "12345").unwrap();
        assert_eq!(memory_limit(dir.path(), 50000).unwrap(), 12345);
        let mut c = collector(dir.path());
        c.config.host.metrics.clear();
        c.containers.push(Container {
            name: "demo".into(),
            pid: 1,
            cgroup: dir.path().into(),
            memory: GaugeAccumulator::default(),
        });
        c.sample();
        fs::write(dir.path().join("memory.current"), "14000").unwrap();
        c.sample();
        assert_eq!(c.containers[0].memory.take(), Some([10000.0, 0.0]));
        fs::remove_file(dir.path().join("memory.current")).unwrap();
        c.sample();
        assert!(c.containers.is_empty());
        let result = command_with_timeout(&["sleep".into(), "2".into()], Duration::from_millis(60));
        assert_eq!(result.unwrap_err(), "command timed out");
    }
    fn collector(dir: &Path) -> Collector {
        let text = format!("[collector]\ninterval_secs=300\nsample_secs=10\ndata_dir='{}'\n[host]\nmetrics=['mem_swap','loadavg','network','disk_io']\ninterfaces=['eth0']\ndisks=['sda']\ndisk_usage_path='/'\n[containers]\nengine='none'\n", dir.display());
        let config = toml::from_str(&text).unwrap();
        Collector::new(config, PathBuf::from("tests/fixtures/proc"))
    }
    #[test]
    fn collector_fixture_values_and_selection() {
        let d = tempfile::tempdir().unwrap();
        let mut c = collector(d.path());
        assert_eq!(c.memory().unwrap(), [614400.0, 153600.0]);
        assert_eq!(c.load().unwrap(), [1.0, 2.0]);
        assert_eq!(c.network().unwrap(), [100, 200]);
        assert_eq!(c.disk_io().unwrap(), [1536, 3584]);
        assert_eq!(c.cpu().unwrap(), [100, 20, 30, 400, 50, 60, 70, 80]);
        c.sample();
        assert_eq!(c.cycle(), (4, 0));
        assert!(!d.path().join("uptime.rrd").exists());
    }
    #[test]
    fn collector_samples_ignore_failures_and_reset() {
        let d = tempfile::tempdir().unwrap();
        let mut c = collector(d.path());
        c.config.host.metrics = vec!["mem_swap".into()];
        c.proc_root = d.path().into();
        for used in [100, 200, 300] {
            fs::write(
                d.path().join("meminfo"),
                format!(
                    "MemTotal: 1000 kB\nMemAvailable: {} kB\nSwapTotal: 100 kB\nSwapFree: 50 kB\n",
                    1000 - used
                ),
            )
            .unwrap();
            c.sample();
        }
        fs::remove_file(d.path().join("meminfo")).unwrap();
        c.sample();
        assert_eq!(
            c.gauges.get_mut("mem_swap").unwrap().take(),
            Some([204800.0, 51200.0])
        );
        assert_eq!(c.gauges.get_mut("mem_swap").unwrap().take(), None);
    }
    #[test]
    fn collector_boundary_values_and_cpu_previous() {
        let d = tempfile::tempdir().unwrap();
        let mut c = collector(d.path());
        c.config.host.metrics = vec!["cpu".into(), "uptime".into(), "disk_usage".into()];
        c.cpu_previous = Some(c.cpu().unwrap());
        let root = d.path().join("proc");
        fs::create_dir(&root).unwrap();
        fs::copy("tests/fixtures/proc/stat.second", root.join("stat")).unwrap();
        fs::copy("tests/fixtures/proc/uptime", root.join("uptime")).unwrap();
        c.proc_root = root;
        assert_eq!(c.cycle(), (5, 0));
        assert!(d.path().join("cpu_user_idle.rrd").exists());
        assert!(d.path().join("uptime.rrd").exists());
        assert!(d.path().join("disk_usage.rrd").exists());
    }
    #[test]
    fn collector_first_cpu_cycle_uses_one_second_sample() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("proc");
        fs::create_dir(&root).unwrap();
        fs::copy("tests/fixtures/proc/stat", root.join("stat")).unwrap();
        let stat = root.join("stat");
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            fs::copy("tests/fixtures/proc/stat.second", stat).unwrap();
        });
        let mut c = collector(d.path());
        c.config.host.metrics = vec!["cpu".into()];
        c.proc_root = root;
        assert_eq!(c.cycle(), (3, 0));
        writer.join().unwrap();
    }
    #[test]
    fn collector_counters_read_only_at_boundary() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("proc");
        fs::create_dir_all(root.join("net")).unwrap();
        fs::copy("tests/fixtures/proc/net/dev", root.join("net/dev")).unwrap();
        fs::copy("tests/fixtures/proc/diskstats", root.join("diskstats")).unwrap();
        let mut c = collector(d.path());
        c.config.host.metrics = vec!["network".into(), "disk_io".into()];
        c.proc_root = root.clone();
        c.sample();
        fs::write(
            root.join("net/dev"),
            "header\nheader\neth0: 300 0 0 0 0 0 0 0 400 0 0 0 0 0 0 0\n",
        )
        .unwrap();
        fs::write(root.join("diskstats"), "8 0 sda 1 2 5 4 5 6 9 8\n").unwrap();
        assert_eq!(c.cycle(), (2, 0));
        for (name, values) in [("network", "300 400"), ("disk_io", "2560 4608")] {
            let output = Command::new("rrdtool")
                .env("LC_ALL", "C")
                .arg("lastupdate")
                .arg(d.path().join(format!("{name}.rrd")))
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(String::from_utf8(output.stdout).unwrap().contains(values));
        }
    }
    #[test]
    fn collector_uses_boundary_value_without_samples() {
        let d = tempfile::tempdir().unwrap();
        let mut c = collector(d.path());
        c.config.collector.sample_secs = 0;
        c.config.host.metrics = vec!["loadavg".into()];
        assert_eq!(c.cycle(), (1, 0));
        let output = Command::new("rrdtool")
            .env("LC_ALL", "C")
            .arg("lastupdate")
            .arg(d.path().join("loadavg.rrd"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let result = String::from_utf8(output.stdout).unwrap();
        assert!(result.contains("1 2"), "{result}");
    }
    #[test]
    fn collector_thirty_samples_written_and_reset() {
        let d = tempfile::tempdir().unwrap();
        let mut c = collector(d.path());
        c.config.host.metrics = vec!["loadavg".into()];
        c.proc_root = d.path().into();
        for i in 1..=30 {
            fs::write(d.path().join("loadavg"), format!("{i} {} 0 1/1 1\n", i * 2)).unwrap();
            c.sample();
        }
        assert_eq!(c.gauges["loadavg"].count, 30);
        assert_eq!(c.cycle(), (1, 0));
        assert_eq!(c.gauges["loadavg"].count, 0);
        let output = Command::new("rrdtool")
            .env("LC_ALL", "C")
            .arg("lastupdate")
            .arg(d.path().join("loadavg.rrd"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .contains("15.5 31"));
    }
    #[test]
    fn collector_accumulator_reset() {
        let mut a = GaugeAccumulator::default();
        for i in 1..=30 {
            a.add([i as f64, (i * 2) as f64]);
        }
        assert_eq!(a.take(), Some([15.5, 31.0]));
        assert_eq!(a.take(), None);
    }
}
