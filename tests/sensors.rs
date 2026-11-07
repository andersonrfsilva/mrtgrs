use mrtg_rs::{collector::Collector, config::Config};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn last_update(dir: &Path, name: &str) -> Vec<f64> {
    let output = Command::new("rrdtool")
        .arg("lastupdate")
        .arg(dir.join(format!("{name}.rrd")))
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    text.lines()
        .last()
        .unwrap()
        .split_once(':')
        .unwrap()
        .1
        .split_whitespace()
        .map(|value| {
            if value == "U" {
                f64::NAN
            } else {
                value.parse().unwrap()
            }
        })
        .collect()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn sensor_collection_contract() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let hwmon = root.join("sys/class/hwmon");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sensors");
    copy_tree(&fixtures.join("hwmon"), &hwmon);

    let smartctl = root.join("smartctl");
    fs::write(
        &smartctl,
        format!(
            r#"#!/bin/sh
/bin/cat '{}/'"${{6##*/}}.json"
case "$6" in
  /dev/sdc) exit 8 ;;
  /dev/sdd) exit 2 ;;
esac
"#,
            fixtures.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&smartctl, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![root.to_path_buf()];
    paths.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

    let config: Config = toml::from_str(&format!(
        "[collector]\ninterval_secs=300\nsample_secs=10\ndata_dir='{}'\n[host]\nmetrics=['temperature','smart']\ndisks=['sda','sdb','nvme0n1','sdc','sdd','sde']\ndisk_usage_path='/'\n[containers]\nengine='none'\n",
        root.display()
    )).unwrap();
    let mut collector = Collector::new(config.clone(), root.join("proc"));
    collector.sys_root = root.join("sys");
    collector.sample();
    fs::write(hwmon.join("hwmon0/temp1_input"), "60000").unwrap();
    fs::write(hwmon.join("hwmon0/temp2_input"), "80000").unwrap();
    collector.sample();
    assert_eq!(collector.cycle(), (10, 2));

    for (name, expected) in [
        ("temp_coretemp_s1", vec![50.0, 0.0]),
        ("temp_coretemp_s2", vec![70.0, 0.0]),
        ("temp_coretemp_2_s1", vec![30.0, 0.0]),
        ("temp_acpitz_s1", vec![45.0, 0.0]),
        ("temp_disco_sda", vec![42.0, 0.0]),
        ("smart_sda", vec![3.0, 9.0]),
        // 197/198 ausentes: desconhecido, não zero.
        ("smart_sdb", vec![7.0, f64::NAN]),
        ("temp_disco_nvme0n1", vec![38.0, 0.0]),
        ("smart_nvme0n1", vec![2.0, 1.0]),
        ("temp_disco_sdc", vec![50.0, 0.0]),
    ] {
        assert_eq!(
            format!("{:?}", last_update(root, name)),
            format!("{expected:?}"),
            "{name}"
        );
    }
    for name in [
        // One RRD per sensor, no per-chip aggregate.
        "temp_coretemp",
        "temp_nvme_s1",
        "temp_drivetemp_s1",
        "temp_broken_s1",
        // Chip sem sensor de temperatura: ignorado sem erro.
        "temp_hp_s1",
        // Tabela ATA vazia: nada medido, nada gravado.
        "smart_sdc",
        "smart_sdd",
        "smart_sde",
    ] {
        assert!(!root.join(format!("{name}.rrd")).exists(), "{name}");
    }

    let boundary = tempfile::tempdir().unwrap();
    let mut config = config;
    config.collector.data_dir = boundary.path().into();
    config.collector.sample_secs = 0;
    config.host.metrics = vec!["temperature".into()];
    let mut collector = Collector::new(config.clone(), root.join("proc"));
    collector.sys_root = root.join("sys");
    assert_eq!(collector.cycle(), (4, 0));
    assert_eq!(
        last_update(boundary.path(), "temp_coretemp_s2"),
        vec![80.0, 0.0]
    );

    config.host.metrics = vec!["smart".into()];
    std::env::set_var("PATH", boundary.path());
    let mut collector = Collector::new(config, root.join("proc"));
    assert_eq!(collector.cycle(), (0, 1));
    std::env::set_var("PATH", path);
}
