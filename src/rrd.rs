use libc::{c_char, c_int, c_long, c_ulong, c_void, time_t};
use serde::Serialize;
use std::{
    collections::HashMap,
    ffi::{CStr, CString},
    os::unix::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::Mutex,
};

static RRD_LOCK: Mutex<()> = Mutex::new(());
#[repr(C)]
struct RrdTimeValue {
    ty: c_int,
    offset: c_long,
    tm: libc::tm,
}
extern "C" {
    fn rrd_clear_error();
    fn rrd_parsetime(spec: *const c_char, value: *mut RrdTimeValue) -> *mut c_char;
    fn rrd_proc_start_end(
        start: *mut RrdTimeValue,
        end: *mut RrdTimeValue,
        start_out: *mut time_t,
        end_out: *mut time_t,
    ) -> c_int;
    fn rrd_fetch_r(
        path: *const c_char,
        cf: *const c_char,
        start: *mut time_t,
        end: *mut time_t,
        step: *mut c_ulong,
        count: *mut c_ulong,
        names: *mut *mut *mut c_char,
        data: *mut *mut f64,
    ) -> c_int;
    fn rrd_freemem(ptr: *mut c_void);
    fn rrd_get_error() -> *const c_char;
    fn rrd_create_r2(
        path: *const c_char,
        step: c_ulong,
        last_up: time_t,
        no_overwrite: c_int,
        sources: *const *const c_char,
        template: *const c_char,
        argc: c_int,
        argv: *const *const c_char,
    ) -> c_int;
    fn rrd_updatex_r(
        path: *const c_char,
        template: *const c_char,
        flags: c_int,
        argc: c_int,
        argv: *const *const c_char,
    ) -> c_int;
}
#[derive(Debug)]
pub enum RrdError {
    Execution,
}
pub struct RawFetch {
    pub ds_names: Vec<String>,
    pub timestamps: Vec<i64>,
    pub series: HashMap<String, Vec<Option<f64>>>,
}

pub fn create_rrd(path: &Path, kinds: &[&str]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut args: Vec<String> = kinds
        .iter()
        .enumerate()
        .map(|(i, kind)| format!("DS:ds{i}:{kind}:900:0:U"))
        .collect();
    args.extend(
        [
            "RRA:AVERAGE:0.5:1:2016",
            "RRA:AVERAGE:0.5:12:8760",
            "RRA:MAX:0.5:12:8760",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let args: Vec<CString> = args
        .iter()
        .map(|arg| CString::new(arg.as_str()).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let argv: Vec<*const c_char> = args.iter().map(|arg| arg.as_ptr()).collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as time_t;
    let _guard = RRD_LOCK.lock().map_err(|e| e.to_string())?;
    unsafe {
        rrd_clear_error();
        if rrd_create_r2(
            path.as_ptr(),
            300,
            now - 10,
            1,
            ptr::null(),
            ptr::null(),
            argv.len() as c_int,
            argv.as_ptr(),
        ) != 0
        {
            return Err(CStr::from_ptr(rrd_get_error())
                .to_string_lossy()
                .into_owned());
        }
    }
    Ok(())
}

pub fn update_rrd(path: &Path, values: &[String], kinds: &[&str]) -> Result<(), String> {
    if !path.exists() {
        create_rrd(path, kinds)?;
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let value = CString::new(format!("N:{}", values.join(":"))).map_err(|e| e.to_string())?;
    let argv = [value.as_ptr()];
    let _guard = RRD_LOCK.lock().map_err(|e| e.to_string())?;
    unsafe {
        rrd_clear_error();
        if rrd_updatex_r(path.as_ptr(), ptr::null(), 0, 1, argv.as_ptr()) != 0 {
            return Err(CStr::from_ptr(rrd_get_error())
                .to_string_lossy()
                .into_owned());
        }
    }
    Ok(())
}

pub fn fetch_rrd(path: &Path, start: &str, end: &str) -> Result<RawFetch, RrdError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| RrdError::Execution)?;
    let start = CString::new(start).map_err(|_| RrdError::Execution)?;
    let end = CString::new(end).map_err(|_| RrdError::Execution)?;
    let _guard = RRD_LOCK.lock().map_err(|_| RrdError::Execution)?;
    unsafe {
        rrd_clear_error();
        let mut st: RrdTimeValue = std::mem::zeroed();
        let mut et: RrdTimeValue = std::mem::zeroed();
        if !rrd_parsetime(start.as_ptr(), &mut st).is_null()
            || !rrd_parsetime(end.as_ptr(), &mut et).is_null()
        {
            return Err(RrdError::Execution);
        }
        let (mut s, mut e) = (0, 0);
        if rrd_proc_start_end(&mut st, &mut et, &mut s, &mut e) < 0 {
            return Err(RrdError::Execution);
        }
        let (mut step, mut count) = (0, 0);
        let mut names: *mut *mut c_char = ptr::null_mut();
        let mut data: *mut f64 = ptr::null_mut();
        let rc = rrd_fetch_r(
            path.as_ptr(),
            c"AVERAGE".as_ptr(),
            &mut s,
            &mut e,
            &mut step,
            &mut count,
            &mut names,
            &mut data,
        );
        if rc != 0 || step == 0 {
            if !names.is_null() {
                for i in 0..count as usize {
                    rrd_freemem((*names.add(i)).cast());
                }
                rrd_freemem(names.cast());
            }
            if !data.is_null() {
                rrd_freemem(data.cast());
            }
            return Err(RrdError::Execution);
        }
        let rows = ((e - s) / step as time_t).max(0) as usize;
        let mut ds_names = Vec::new();
        for i in 0..count as usize {
            let p = *names.add(i);
            ds_names.push(CStr::from_ptr(p).to_string_lossy().into_owned());
            rrd_freemem(p.cast());
        }
        rrd_freemem(names.cast());
        let timestamps = (0..rows)
            .map(|i| s + (i as i64 + 1) * step as i64)
            .collect();
        let mut series = HashMap::new();
        for (j, name) in ds_names.iter().enumerate() {
            let values = (0..rows)
                .map(|i| {
                    let v = *data.add(i * count as usize + j);
                    v.is_finite().then_some(v)
                })
                .collect();
            series.insert(name.clone(), values);
        }
        rrd_freemem(data.cast());
        Ok(RawFetch {
            ds_names,
            timestamps,
            series,
        })
    }
}
#[derive(Serialize)]
pub struct Stats {
    min: Option<f64>,
    max: Option<f64>,
    avg: Option<f64>,
    current: Option<f64>,
}
#[derive(Serialize)]
pub struct QueryPayload {
    target: String,
    step: i64,
    timestamps: Vec<i64>,
    series: HashMap<String, Vec<Option<f64>>>,
    stats: HashMap<String, Stats>,
}
pub fn process_series(
    target: &str,
    mut ds_names: Vec<String>,
    mut series: HashMap<String, Vec<Option<f64>>>,
    mut timestamps: Vec<i64>,
    max_points: usize,
) -> QueryPayload {
    while !timestamps.is_empty() && ds_names.iter().all(|ds| series[ds].last() == Some(&None)) {
        timestamps.pop();
        for ds in &ds_names {
            series.get_mut(ds).unwrap().pop();
        }
    }
    // Temperatures only use ds0; old per-chip RRDs kept the average in ds1.
    if (target == "uptime"
        || target.starts_with("temp_")
        || (target.starts_with("docker_") && target.ends_with("_cpu")))
        && ds_names.iter().any(|v| v == "ds0")
    {
        ds_names.retain(|v| v == "ds0");
        series.retain(|k, _| k == "ds0");
    }
    let step = if timestamps.len() >= 2 {
        timestamps[1] - timestamps[0]
    } else {
        300
    };
    let n = timestamps.len();
    if n > max_points && max_points > 1 {
        let mut ts = Vec::with_capacity(max_points);
        let mut output: HashMap<String, Vec<Option<f64>>> = ds_names
            .iter()
            .map(|ds| (ds.clone(), Vec::with_capacity(max_points)))
            .collect();
        for i in 0..max_points {
            let a = i * n / max_points;
            let b = ((i + 1) * n / max_points).max(a + 1);
            ts.push(
                timestamps[if i == 0 {
                    0
                } else if i + 1 == max_points {
                    n - 1
                } else {
                    (a + b - 1) / 2
                }],
            );
            for ds in &ds_names {
                let values: Vec<f64> = series[ds][a..b].iter().flatten().copied().collect();
                output.get_mut(ds).unwrap().push(
                    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64),
                );
            }
        }
        timestamps = ts;
        series = output;
    }
    let stats = ds_names
        .iter()
        .map(|ds| {
            let values: Vec<f64> = series[ds].iter().flatten().copied().collect();
            let stat = Stats {
                min: values.iter().copied().reduce(f64::min),
                max: values.iter().copied().reduce(f64::max),
                avg: (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64),
                current: values.last().copied(),
            };
            (ds.clone(), stat)
        })
        .collect();
    QueryPayload {
        target: target.to_owned(),
        step,
        timestamps,
        series,
        stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn rrd_write_matches_cli_definition() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join("native.rrd");
        let cli = dir.path().join("cli.rrd");
        create_rrd(&native, &["GAUGE", "DERIVE"]).unwrap();
        assert!(Command::new("rrdtool")
            .env("LC_ALL", "C")
            .args([
                "create",
                cli.to_str().unwrap(),
                "--step",
                "300",
                "DS:ds0:GAUGE:900:0:U",
                "DS:ds1:DERIVE:900:0:U",
                "RRA:AVERAGE:0.5:1:2016",
                "RRA:AVERAGE:0.5:12:8760",
                "RRA:MAX:0.5:12:8760"
            ])
            .status()
            .unwrap()
            .success());
        let info = |path: &Path| {
            let output = Command::new("rrdtool")
                .env("LC_ALL", "C")
                .arg("info")
                .arg(path)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .filter(|line| {
                    line.starts_with("step =")
                        || (line.starts_with("ds[")
                            && [".type =", ".minimal_heartbeat =", ".min =", ".max ="]
                                .iter()
                                .any(|part| line.contains(part)))
                        || (line.starts_with("rra[")
                            && [".cf =", ".pdp_per_row =", ".rows =", ".xff ="]
                                .iter()
                                .any(|part| line.contains(part)))
                })
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(info(&native), info(&cli));
        update_rrd(&native, &["1".into(), "2".into()], &["GAUGE", "DERIVE"]).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        update_rrd(&native, &["2".into(), "3".into()], &["GAUGE", "DERIVE"]).unwrap();
    }

    #[test]
    fn process_trims_and_filters() {
        let names = vec!["ds0".to_string(), "ds1".to_string()];
        let series = HashMap::from([
            ("ds0".to_string(), vec![Some(1.0), Some(3.0), None]),
            ("ds1".to_string(), vec![None, None, None]),
        ]);
        let p = process_series("uptime", names, series, vec![300, 600, 900], 2000);
        assert_eq!(p.timestamps, vec![300, 600]);
        assert_eq!(p.step, 300);
        assert_eq!(p.series.len(), 1);
        assert_eq!(p.stats["ds0"].avg, Some(2.0));
    }

    #[test]
    fn fetch_real_rrd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rrd");
        assert!(Command::new("rrdtool")
            .env("LC_ALL", "C")
            .args([
                "create",
                path.to_str().unwrap(),
                "--start",
                "1000000000",
                "--step",
                "300",
                "DS:ds0:GAUGE:600:U:U",
                "RRA:AVERAGE:0.5:1:100"
            ])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("rrdtool")
            .env("LC_ALL", "C")
            .args([
                "update",
                path.to_str().unwrap(),
                "1000000200:1",
                "1000000500:2"
            ])
            .status()
            .unwrap()
            .success());
        let result = fetch_rrd(&path, "1000000000", "1000000800").unwrap();
        assert_eq!(result.ds_names, ["ds0"]);
        assert!(!result.timestamps.is_empty());
        assert!(result.series["ds0"].iter().any(Option::is_some));
    }

    #[test]
    fn normal_gaps_and_single_metric() {
        for target in ["loadavg", "uptime", "docker_chatwoot_cpu", "temp_disco_sda"] {
            let names = vec!["ds0".into(), "ds1".into()];
            let data = HashMap::from([
                ("ds0".into(), vec![Some(1.0), None, Some(3.0), None]),
                ("ds1".into(), vec![None, None, None, None]),
            ]);
            let payload = process_series(target, names, data, vec![300, 600, 900, 1200], 2000);
            assert_eq!(payload.timestamps, vec![300, 600, 900]);
            assert_eq!(payload.series["ds0"], vec![Some(1.0), None, Some(3.0)]);
            let stats = &payload.stats["ds0"];
            assert_eq!(
                (stats.min, stats.max, stats.avg, stats.current),
                (Some(1.0), Some(3.0), Some(2.0), Some(3.0))
            );
            if target == "loadavg" {
                let stats = &payload.stats["ds1"];
                assert_eq!(
                    (stats.min, stats.max, stats.avg, stats.current),
                    (None, None, None, None)
                );
                assert_eq!(
                    serde_json::to_value(&payload).unwrap()["series"]["ds1"][0],
                    serde_json::Value::Null
                );
            } else {
                assert!(!payload.series.contains_key("ds1"));
                assert!(!payload.stats.contains_key("ds1"));
            }
        }
    }

    #[test]
    fn downsample_endpoints() {
        let timestamps: Vec<i64> = (0..5000).collect();
        let series = HashMap::from([("ds0".into(), vec![Some(1.0); 5000])]);
        let payload = process_series("loadavg", vec!["ds0".into()], series, timestamps, 2000);
        assert_eq!(payload.timestamps.len(), 2000);
        assert_eq!((payload.timestamps[0], payload.timestamps[1999]), (0, 4999));
        assert_eq!(payload.step, 1);
    }

    #[test]
    fn parity_with_rrdtool_cli() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parity.rrd");
        let path = path.to_str().unwrap();
        assert!(Command::new("rrdtool")
            .env("LC_ALL", "C")
            .args([
                "create",
                path,
                "--start",
                "1000000000",
                "--step",
                "300",
                "DS:ds0:GAUGE:600:U:U",
                "DS:ds1:GAUGE:600:U:U",
                "RRA:AVERAGE:0.5:1:100"
            ])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("rrdtool")
            .env("LC_ALL", "C")
            .args(["update", path, "1000000200:1:U", "1000000500:2:3"])
            .status()
            .unwrap()
            .success());
        let result = fetch_rrd(Path::new(path), "1000000000", "1000000800").unwrap();
        let cli = Command::new("rrdtool")
            .env("LC_ALL", "C")
            .args([
                "fetch",
                path,
                "AVERAGE",
                "--start",
                "1000000000",
                "--end",
                "1000000800",
            ])
            .output()
            .unwrap();
        assert!(cli.status.success());
        let output = String::from_utf8(cli.stdout).unwrap();
        let mut seen_null = false;
        for (i, line) in output.lines().filter(|line| line.contains(':')).enumerate() {
            let (timestamp, values) = line.trim().split_once(':').unwrap();
            assert_eq!(result.timestamps[i], timestamp.parse::<i64>().unwrap());
            for (name, text) in result.ds_names.iter().zip(values.split_whitespace()) {
                let number = if text.to_ascii_lowercase().contains("nan") {
                    f64::NAN
                } else {
                    text.parse::<f64>().unwrap()
                };
                let actual = result.series[name][i];
                if !number.is_finite() {
                    assert_eq!(actual, None);
                    seen_null = true;
                } else {
                    assert!((actual.unwrap() - number).abs() < 1e-6);
                }
            }
        }
        assert!(seen_null);
    }
}
