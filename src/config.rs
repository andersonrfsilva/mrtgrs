use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub collector: Collector,
    pub host: Host,
    pub containers: Containers,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Collector {
    pub interval_secs: u64,
    #[serde(default)]
    pub sample_secs: u64,
    pub data_dir: PathBuf,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Host {
    pub metrics: Vec<String>,
    #[serde(default)]
    pub interfaces: Vec<String>,
    #[serde(default)]
    pub disks: Vec<String>,
    pub disk_usage_path: PathBuf,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Containers {
    pub engine: String,
    #[serde(default)]
    pub rootless: Vec<Rootless>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Rootless {
    pub user: String,
    pub uid: u32,
}

impl Config {
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let config: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.validate()?;
        Ok(Some(config))
    }
    pub fn validate(&self) -> Result<(), String> {
        const METRICS: &[&str] = &[
            "cpu",
            "mem_swap",
            "loadavg",
            "uptime",
            "disk_usage",
            "network",
            "disk_io",
            "temperature",
            "smart",
        ];
        for metric in &self.host.metrics {
            if !METRICS.contains(&metric.as_str()) {
                return Err(format!("unknown metric: {metric}"));
            }
        }
        if self.host.metrics.iter().any(|m| m == "network") && self.host.interfaces.is_empty() {
            return Err("network requires interfaces".into());
        }
        if self.host.metrics.iter().any(|m| m == "disk_io") && self.host.disks.is_empty() {
            return Err("disk_io requires disks".into());
        }
        if self.host.metrics.iter().any(|m| m == "smart") && self.host.disks.is_empty() {
            return Err("smart requires disks".into());
        }
        let interval = self.collector.interval_secs;
        let sample = self.collector.sample_secs;
        if interval < 60 {
            return Err("interval_secs must be at least 60".into());
        }
        if sample != 0 && (sample < 5 || !interval.is_multiple_of(sample)) {
            return Err("sample_secs must be at least 5 and divide interval_secs".into());
        }
        if !["podman", "docker", "none"].contains(&self.containers.engine.as_str()) {
            return Err("engine must be podman, docker or none".into());
        }
        // shortcut: rootless Docker needs DOCKER_HOST per user; add when a host runs it.
        if self.containers.engine == "docker" && !self.containers.rootless.is_empty() {
            return Err("rootless is only supported with podman".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const VALID: &str = "[collector]\ninterval_secs=300\ndata_dir='/tmp/data'\n[host]\nmetrics=['network','disk_io']\ninterfaces=['eth0']\ndisks=['sda']\ndisk_usage_path='/'\n[containers]\nengine='none'\n";
    #[test]
    fn config_valid_and_missing() {
        let d = tempfile::tempdir().unwrap();
        assert!(Config::load(&d.path().join("missing")).unwrap().is_none());
        let p = d.path().join("config.toml");
        fs::write(&p, VALID).unwrap();
        assert_eq!(Config::load(&p).unwrap().unwrap().collector.sample_secs, 0);
    }
    #[test]
    fn config_rejects_invalid() {
        for text in [
            VALID.replace("'network'", "'invalid'"),
            VALID.replace("interfaces=['eth0']", "interfaces=[]"),
            VALID.replace("disks=['sda']", "disks=[]"),
            VALID
                .replace("'network','disk_io'", "'smart'")
                .replace("disks=['sda']", "disks=[]"),
            VALID.replace("interval_secs=300", "interval_secs=59"),
            VALID.replace("interval_secs=300", "interval_secs=300\nsample_secs=4"),
            VALID.replace("interval_secs=300", "interval_secs=300\nsample_secs=7"),
            VALID.replace("engine='none'", "engine='lxc'"),
            VALID.replace(
                "engine='none'",
                "engine='docker'\nrootless=[{user='u',uid=1000}]",
            ),
        ] {
            assert!(toml::from_str::<Config>(&text).unwrap().validate().is_err());
        }
    }
    #[test]
    fn accepts_temperature_and_smart() {
        let text = VALID.replace("'network','disk_io'", "'temperature','smart'");
        assert!(toml::from_str::<Config>(&text).unwrap().validate().is_ok());
        let text = VALID.replace("engine='none'", "engine='docker'");
        assert!(toml::from_str::<Config>(&text).unwrap().validate().is_ok());
    }
}
