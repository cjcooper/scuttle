//! Container resource sampling for the Resources page.
//!
//! While the dashboard polls (`stats_poll`), a background task samples
//! `docker stats --no-stream` (about every 3 s; the call itself waits ~2 s
//! for two CPU readings) and keeps the last 15 minutes in memory, so graphs
//! have history as soon as the page opens. Sampling stops once nobody has
//! polled for a while, and history is dropped when the selected engine
//! changes.
//!
//! Compose membership comes from the `com.docker.compose.project` /
//! `.service` labels, so stacks started outside the app group too.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::engine::Docker;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// How much history to keep.
const RETENTION: Duration = Duration::from_secs(15 * 60);
/// Pause between samples (on top of the ~2 s `docker stats` takes).
const INTERVAL: Duration = Duration::from_secs(1);
/// Stop sampling when no one has polled for this long.
const IDLE: Duration = Duration::from_secs(20);

/// One container's usage at one instant.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSample {
    pub id: String,
    /// Percent of one CPU (docker's convention: 200 = two cores busy).
    pub cpu: f64,
    pub mem: u64,
    pub mem_limit: u64,
    /// Bytes per second since the previous sample (0 for the first one).
    pub net_rx: f64,
    pub net_tx: f64,
    pub blk_read: f64,
    pub blk_write: f64,
    pub pids: u64,
    /// Cumulative counters, for rates and totals.
    #[serde(skip)]
    totals: Counters,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Counters {
    net_rx: u64,
    net_tx: u64,
    blk_read: u64,
    blk_write: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub seq: u64,
    /// Unix milliseconds.
    pub t: u64,
    pub containers: Vec<ContainerSample>,
}

/// A container (running or not) and its compose labels.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub project: String,
    pub service: String,
    pub working_dir: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsBatch {
    /// Pass back as `since` to get only newer samples.
    pub seq: u64,
    /// True when `since` was older than the buffer (or from another engine):
    /// the client should replace its history with `samples`.
    pub reset: bool,
    pub ncpu: u32,
    pub mem_total: u64,
    pub interval_ms: u64,
    pub samples: Vec<Sample>,
    pub members: Vec<Member>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Inner {
    /// Bumped when history is dropped; clients holding an older epoch reset.
    epoch: u64,
    seq: u64,
    samples: VecDeque<Sample>,
    members: Vec<Member>,
    ncpu: u32,
    mem_total: u64,
    error: Option<String>,
    /// provider/profile the history belongs to.
    engine: String,
    running: bool,
    last_poll: Option<Instant>,
}

#[derive(Default, Clone)]
pub struct Stats(Arc<Mutex<Inner>>);

/// `seq` values carry the epoch in the high bits so a client's cursor from
/// before an engine switch never matches.
const EPOCH_SHIFT: u32 = 40;

impl Stats {
    fn poll(&self, app: &AppHandle, since: u64) -> StatsBatch {
        let engine = engine_key(app);
        let start = {
            let mut g = self.0.lock().unwrap();
            if g.engine != engine {
                g.epoch += 1;
                g.seq = g.epoch << EPOCH_SHIFT;
                g.samples.clear();
                g.members.clear();
                g.error = None;
                g.ncpu = 0;
                g.mem_total = 0;
                g.engine = engine;
            }
            g.last_poll = Some(Instant::now());
            !std::mem::replace(&mut g.running, true)
        };
        if start {
            self.spawn(app.clone());
        }
        let g = self.0.lock().unwrap();
        let oldest = g.samples.front().map(|s| s.seq).unwrap_or(g.seq + 1);
        let same_epoch = since >> EPOCH_SHIFT == g.epoch;
        let reset = !same_epoch || since + 1 < oldest;
        StatsBatch {
            seq: g.seq,
            reset,
            ncpu: g.ncpu,
            mem_total: g.mem_total,
            interval_ms: (INTERVAL + Duration::from_secs(2)).as_millis() as u64,
            samples: g
                .samples
                .iter()
                .filter(|s| reset || s.seq > since)
                .cloned()
                .collect(),
            members: g.members.clone(),
            error: g.error.clone(),
        }
    }

    fn spawn(&self, app: AppHandle) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let idle = {
                    let g = this.0.lock().unwrap();
                    g.last_poll.map_or(true, |t| t.elapsed() > IDLE)
                };
                if idle {
                    this.0.lock().unwrap().running = false;
                    return;
                }
                this.sample_once(&app).await;
                tokio::time::sleep(INTERVAL).await;
            }
        });
    }

    async fn sample_once(&self, app: &AppHandle) {
        let engine = engine_key(app);
        let state = app.state::<AppState>();
        let up = state
            .status()
            .await
            .map(|s| s.engine_version.is_some() && s.busy.is_none())
            .unwrap_or(false);
        if !up {
            self.0.lock().unwrap().error = Some("Engine not running".into());
            tokio::time::sleep(Duration::from_secs(2)).await;
            return;
        }
        let docker = match state.docker().await {
            Ok(d) => d,
            Err(e) => {
                self.0.lock().unwrap().error = Some(e.to_string());
                return;
            }
        };
        let need_info = self.0.lock().unwrap().ncpu == 0;
        let info = async {
            if need_info {
                engine_info(&docker).await.ok()
            } else {
                None
            }
        };
        let (stats, members, info) = tokio::join!(stats(&docker), members(&docker), info);
        let t = now_ms();

        let mut g = self.0.lock().unwrap();
        if g.engine != engine {
            // The selection changed mid-sample; this data is for the old one.
            return;
        }
        if let Some((ncpu, mem)) = info {
            g.ncpu = ncpu;
            g.mem_total = mem;
        }
        if let Ok(m) = members {
            g.members = m;
        }
        match stats {
            Ok(mut containers) => {
                if let Some(prev) = g.samples.back() {
                    with_rates(&mut containers, prev, t);
                }
                g.error = None;
                g.seq += 1;
                let seq = g.seq;
                g.samples.push_back(Sample { seq, t, containers });
                let cutoff = t.saturating_sub(RETENTION.as_millis() as u64);
                while g.samples.front().is_some_and(|s| s.t < cutoff) {
                    g.samples.pop_front();
                }
            }
            Err(e) => g.error = Some(e.to_string()),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn engine_key(app: &AppHandle) -> String {
    let sel = app.state::<AppState>().selection();
    format!("{}/{}", sel.provider, sel.profile)
}

/// Turn cumulative I/O counters into per-second rates against `prev`.
fn with_rates(now: &mut [ContainerSample], prev: &Sample, t: u64) {
    let by_id: HashMap<&str, &ContainerSample> =
        prev.containers.iter().map(|c| (c.id.as_str(), c)).collect();
    let dt = t.saturating_sub(prev.t) as f64 / 1000.0;
    if dt <= 0.0 {
        return;
    }
    for c in now.iter_mut() {
        let Some(p) = by_id.get(c.id.as_str()) else {
            continue;
        };
        // A restarted container's counters start over; treat that as 0.
        let rate = |a: u64, b: u64| a.saturating_sub(b) as f64 / dt;
        c.net_rx = rate(c.totals.net_rx, p.totals.net_rx);
        c.net_tx = rate(c.totals.net_tx, p.totals.net_tx);
        c.blk_read = rate(c.totals.blk_read, p.totals.blk_read);
        c.blk_write = rate(c.totals.blk_write, p.totals.blk_write);
    }
}

// ------------------------------------------------------------ docker CLI

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawStats {
    #[serde(rename = "ID")]
    id: String,
    #[serde(default, rename = "CPUPerc")]
    cpu_perc: String,
    #[serde(default)]
    mem_usage: String,
    #[serde(default, rename = "NetIO")]
    net_io: String,
    #[serde(default, rename = "BlockIO")]
    block_io: String,
    #[serde(default, rename = "PIDs")]
    pids: String,
}

fn parse_stats_line(line: &str) -> Option<ContainerSample> {
    let r: RawStats = serde_json::from_str(line.trim()).ok()?;
    let pair = |s: &str| -> (u64, u64) {
        let (a, b) = s.split_once('/').unwrap_or((s, ""));
        (
            parse_size(a).unwrap_or_default(),
            parse_size(b).unwrap_or_default(),
        )
    };
    let (mem, mem_limit) = pair(&r.mem_usage);
    let (net_rx, net_tx) = pair(&r.net_io);
    let (blk_read, blk_write) = pair(&r.block_io);
    Some(ContainerSample {
        id: r.id,
        cpu: r
            .cpu_perc
            .trim()
            .trim_end_matches('%')
            .parse()
            .unwrap_or(0.0),
        mem,
        mem_limit,
        pids: r.pids.trim().parse().unwrap_or(0),
        totals: Counters {
            net_rx,
            net_tx,
            blk_read,
            blk_write,
        },
        ..Default::default()
    })
}

/// docker's human sizes: `0B`, `4.04kB`, `27.3MB` (powers of 1000) and
/// `31.97MiB`, `62.67GiB` (powers of 1024).
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult: f64 = match unit.trim() {
        "" | "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult).round() as u64)
}

async fn stats(docker: &Docker) -> Result<Vec<ContainerSample>> {
    let cmd = docker.cli(&[
        "container",
        "stats",
        "--no-stream",
        "--format",
        "{{json .}}",
    ])?;
    let out = crate::process::output(cmd, Duration::from_secs(20)).await?;
    Ok(out.lines().filter_map(parse_stats_line).collect())
}

/// Label values can contain commas (config file lists), so ask for each
/// label by name instead of parsing `.Labels`.
const MEMBER_FORMAT: &str = concat!(
    r#"{"id":{{json .ID}},"name":{{json .Names}},"image":{{json .Image}},"#,
    r#""state":{{json .State}},"status":{{json .Status}},"#,
    r#""project":{{json (.Label "com.docker.compose.project")}},"#,
    r#""service":{{json (.Label "com.docker.compose.service")}},"#,
    r#""workingDir":{{json (.Label "com.docker.compose.project.working_dir")}}}"#
);

async fn members(docker: &Docker) -> Result<Vec<Member>> {
    let cmd = docker.cli(&["container", "ls", "--all", "--format", MEMBER_FORMAT])?;
    let out = crate::process::output(cmd, Duration::from_secs(20)).await?;
    Ok(out
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect())
}

async fn engine_info(docker: &Docker) -> Result<(u32, u64)> {
    let cmd = docker.cli(&["info", "--format", "{{.NCPU}} {{.MemTotal}}"])?;
    let out = crate::process::output(cmd, Duration::from_secs(10)).await?;
    let mut it = out.split_whitespace();
    match (
        it.next().and_then(|s| s.parse().ok()),
        it.next().and_then(|s| s.parse().ok()),
    ) {
        (Some(n), Some(m)) => Ok((n, m)),
        _ => Err(AppError::msg(format!(
            "unexpected `docker info` output: {out}"
        ))),
    }
}

// ---------------------------------------------------------------- commands

/// New samples since `since` (0 for everything buffered); keeps the
/// sampler running while the Resources page is open.
#[tauri::command]
pub fn stats_poll(app: AppHandle, since: u64) -> StatsBatch {
    app.state::<Stats>().poll(&app, since)
}

/// `docker container inspect` for the detail view.
#[tauri::command]
pub async fn container_inspect(app: AppHandle, id: String) -> Result<serde_json::Value> {
    let docker = app.state::<AppState>().docker().await?;
    let cmd = docker.cli(&["container", "inspect", &id])?;
    let out = crate::process::output(cmd, crate::process::QUICK).await?;
    let v: serde_json::Value = serde_json::from_str(&out)?;
    v.as_array()
        .and_then(|a| a.first().cloned())
        .ok_or_else(|| AppError::msg(format!("container `{id}` not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(parse_size("4.04kB"), Some(4040));
        assert_eq!(parse_size(" 27.3MB "), Some(27_300_000));
        assert_eq!(
            parse_size("31.97MiB"),
            Some((31.97f64 * 1024.0 * 1024.0).round() as u64)
        );
        assert_eq!(parse_size("16GiB"), Some(17_179_869_184));
        assert_eq!(parse_size("--"), None);
    }

    #[test]
    fn parses_stats_line() {
        let s = parse_stats_line(r#"{"BlockIO":"0B / 12.3kB","CPUPerc":"0.62%","Container":"8045","ID":"8045f6667bf6","MemPerc":"5.32%","MemUsage":"872MiB / 16GiB","Name":"eg_system-db-1","NetIO":"5.18kB / 2.6kB","PIDs":"41"}"#).unwrap();
        assert_eq!(s.id, "8045f6667bf6");
        assert!((s.cpu - 0.62).abs() < 1e-9);
        assert_eq!(s.mem, 872 * 1024 * 1024);
        assert_eq!(s.mem_limit, 16 * 1024 * 1024 * 1024);
        assert_eq!(s.totals.net_rx, 5180);
        assert_eq!(s.totals.blk_write, 12_300);
        assert_eq!(s.pids, 41);
        // A container that is starting reports `--`.
        let z = parse_stats_line(r#"{"ID":"x","CPUPerc":"--","MemUsage":"-- / --","NetIO":"--","BlockIO":"--","PIDs":"--"}"#).unwrap();
        assert_eq!((z.cpu, z.mem, z.pids), (0.0, 0, 0));
    }

    #[test]
    fn computes_rates_and_tolerates_restarts() {
        let c = |id: &str, rx: u64| ContainerSample {
            id: id.into(),
            totals: Counters {
                net_rx: rx,
                ..Default::default()
            },
            ..Default::default()
        };
        let prev = Sample {
            seq: 1,
            t: 10_000,
            containers: vec![c("a", 1000), c("b", 9000)],
        };
        let mut now = vec![c("a", 5000), c("b", 10), c("new", 777)];
        with_rates(&mut now, &prev, 12_000);
        assert_eq!(now[0].net_rx, 2000.0);
        assert_eq!(now[1].net_rx, 0.0); // counter reset after restart
        assert_eq!(now[2].net_rx, 0.0); // no previous sample
    }

    #[test]
    fn parses_member_json() {
        let m: Member = serde_json::from_str(r#"{"id":"9438b6a1c6b6","name":"eg_system-redis-1","image":"redis:alpine","state":"running","status":"Up 21 hours","project":"eg_system","service":"redis","workingDir":"/src/eg"}"#).unwrap();
        assert_eq!(
            (m.project.as_str(), m.service.as_str()),
            ("eg_system", "redis")
        );
    }
}
