//! Colima profile configuration: `$COLIMA_HOME/<profile>/colima.yaml`.
//!
//! The typed [`ColimaConfig`] covers the documented settings. Saving merges
//! it into the existing YAML so keys this app doesn't know about (newer Colima
//! versions) survive. The raw file can also be edited directly. Colima
//! rewrites the file with its own comments the next time it starts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use serde_yaml::{Mapping, Value};

use crate::error::{AppError, Result};

/// `null` in YAML (e.g. `dns: null`) as the type's default.
fn nullable<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(
    d: D,
) -> std::result::Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Kubernetes {
    pub enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub version: String,
    #[serde(deserialize_with = "nullable")]
    pub k3s_args: Vec<String>,
    pub port: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Network {
    pub address: bool,
    #[serde(deserialize_with = "nullable")]
    pub mode: String,
    #[serde(deserialize_with = "nullable")]
    pub interface: String,
    pub preferred_route: bool,
    #[serde(deserialize_with = "nullable")]
    pub dns: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub dns_hosts: BTreeMap<String, String>,
    pub host_addresses: bool,
    #[serde(deserialize_with = "nullable")]
    pub gateway_address: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Mount {
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount_point: Option<String>,
    pub writable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Provision {
    /// `system`, `user`, `after-boot` or `ready`.
    pub mode: String,
    pub script: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ColimaConfig {
    pub cpu: u32,
    /// GiB. Can only grow once the VM exists.
    pub disk: u32,
    /// GiB.
    pub memory: f64,
    /// Root filesystem size, GiB.
    pub root_disk: u32,
    /// Creation-only: `aarch64`, `x86_64`, `host`.
    #[serde(deserialize_with = "nullable")]
    pub arch: String,
    /// Creation-only: `docker`, `containerd`, `incus`.
    #[serde(deserialize_with = "nullable")]
    pub runtime: String,
    /// Creation-only: `vz`, `qemu`, `krunkit`.
    #[serde(deserialize_with = "nullable")]
    pub vm_type: String,
    /// Creation-only: `virtiofs`, `9p`, `sshfs`.
    #[serde(deserialize_with = "nullable")]
    pub mount_type: String,
    #[serde(deserialize_with = "nullable")]
    pub cpu_type: String,
    #[serde(deserialize_with = "nullable")]
    pub model_runner: String,
    #[serde(deserialize_with = "nullable")]
    pub hostname: String,
    pub auto_activate: bool,
    pub forward_agent: bool,
    #[serde(deserialize_with = "nullable")]
    pub port_forwarder: String,
    pub rosetta: bool,
    pub binfmt: bool,
    pub nested_virtualization: bool,
    pub mount_inotify: bool,
    pub ssh_config: bool,
    pub ssh_port: u32,
    #[serde(deserialize_with = "nullable")]
    pub disk_image: String,
    pub kubernetes: Kubernetes,
    pub network: Network,
    #[serde(deserialize_with = "nullable")]
    pub mounts: Vec<Mount>,
    #[serde(deserialize_with = "nullable")]
    pub provision: Vec<Provision>,
    #[serde(deserialize_with = "nullable")]
    pub env: BTreeMap<String, String>,
    /// Docker daemon.json settings.
    #[serde(deserialize_with = "nullable")]
    pub docker: serde_json::Map<String, serde_json::Value>,
}

impl Default for ColimaConfig {
    /// Colima's defaults for a new profile (VZ on macOS).
    fn default() -> Self {
        Self {
            cpu: 2,
            disk: 100,
            memory: 2.0,
            root_disk: 20,
            arch: "host".into(),
            runtime: "docker".into(),
            vm_type: if cfg!(target_os = "macos") {
                "vz"
            } else {
                "qemu"
            }
            .into(),
            mount_type: if cfg!(target_os = "macos") {
                "virtiofs"
            } else {
                "sshfs"
            }
            .into(),
            cpu_type: String::new(),
            model_runner: "docker".into(),
            hostname: String::new(),
            auto_activate: true,
            forward_agent: false,
            port_forwarder: "ssh".into(),
            rosetta: false,
            binfmt: true,
            nested_virtualization: false,
            mount_inotify: false,
            ssh_config: true,
            ssh_port: 0,
            disk_image: String::new(),
            kubernetes: Kubernetes {
                enabled: false,
                version: String::new(),
                k3s_args: vec!["--disable=traefik".into()],
                port: 0,
            },
            network: Network {
                mode: "shared".into(),
                interface: "en0".into(),
                gateway_address: "192.168.5.2".into(),
                ..Default::default()
            },
            mounts: Vec::new(),
            provision: Vec::new(),
            env: BTreeMap::new(),
            docker: serde_json::Map::new(),
        }
    }
}

/// `$COLIMA_HOME`, default `~/.colima`.
fn colima_home() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("COLIMA_HOME") {
        return Ok(PathBuf::from(h));
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".colima"))
        .ok_or_else(|| AppError::msg("HOME is not set"))
}

fn valid_profile(profile: &str) -> Result<()> {
    let ok = !profile.is_empty()
        && profile.len() <= 64
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !profile.starts_with(['.', '-', '_']);
    if ok {
        Ok(())
    } else {
        Err(AppError::msg(
            "profile names may contain letters, digits, '-', '_' and '.', and must start with a letter or digit",
        ))
    }
}

pub fn config_path(profile: &str) -> Result<PathBuf> {
    valid_profile(profile)?;
    Ok(colima_home()?.join(profile).join("colima.yaml"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFile {
    pub path: String,
    pub config: ColimaConfig,
    /// The file as it is on disk.
    pub raw: String,
}

pub fn read(profile: &str) -> Result<ConfigFile> {
    let path = config_path(profile)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| AppError::msg(format!("could not read {}: {e}", path.display())))?;
    let config: ColimaConfig = serde_yaml::from_str(&raw)
        .map_err(|e| AppError::msg(format!("could not parse {}: {e}", path.display())))?;
    Ok(ConfigFile {
        path: path.display().to_string(),
        config,
        raw,
    })
}

/// Recursively merge `new` into `base`; nested maps listed in `MERGE` keep
/// keys the typed config doesn't model, everything else is replaced.
fn merge(base: &mut Mapping, new: Mapping) {
    const MERGE: &[&str] = &["network", "kubernetes"];
    for (k, v) in new {
        let nested = k.as_str().is_some_and(|k| MERGE.contains(&k));
        match (base.get_mut(&k), v) {
            (Some(Value::Mapping(b)), Value::Mapping(n)) if nested => merge(b, n),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

/// Validate a config against the current one (if the VM exists).
pub fn validate(new: &ColimaConfig, current: Option<&ColimaConfig>) -> Result<()> {
    if new.cpu == 0 {
        return Err(AppError::msg("CPUs must be at least 1"));
    }
    if new.memory < 0.5 {
        return Err(AppError::msg("memory must be at least 0.5 GiB"));
    }
    if new.disk == 0 {
        return Err(AppError::msg("disk must be at least 1 GiB"));
    }
    if let Some(cur) = current {
        if new.disk < cur.disk {
            return Err(AppError::msg(format!(
                "disk can only grow once the VM exists (currently {} GiB)",
                cur.disk
            )));
        }
        for (name, a, b) in [
            ("architecture", &new.arch, &cur.arch),
            ("runtime", &new.runtime, &cur.runtime),
            ("VM type", &new.vm_type, &cur.vm_type),
            ("mount type", &new.mount_type, &cur.mount_type),
        ] {
            if a != b {
                return Err(AppError::msg(format!(
                    "{name} can't be changed after the VM is created (delete and recreate the profile)"
                )));
            }
        }
    }
    Ok(())
}

/// Save the typed config into the profile's colima.yaml.
pub fn write(profile: &str, config: &ColimaConfig) -> Result<()> {
    let path = config_path(profile)?;
    let current = read(profile)?;
    validate(config, Some(&current.config))?;
    let mut base = match serde_yaml::from_str::<Value>(&current.raw) {
        Ok(Value::Mapping(m)) => m,
        _ => Mapping::new(),
    };
    let Value::Mapping(new) =
        serde_yaml::to_value(config).map_err(|e| AppError::msg(e.to_string()))?
    else {
        return Err(AppError::msg("config did not serialise to a map"));
    };
    merge(&mut base, new);
    let text =
        serde_yaml::to_string(&Value::Mapping(base)).map_err(|e| AppError::msg(e.to_string()))?;
    std::fs::write(&path, text)?;
    Ok(())
}

/// Save raw YAML (validated by parsing it first).
pub fn write_raw(profile: &str, raw: &str) -> Result<()> {
    let path = config_path(profile)?;
    let current = read(profile)?;
    let parsed: ColimaConfig =
        serde_yaml::from_str(raw).map_err(|e| AppError::msg(format!("invalid YAML: {e}")))?;
    validate(&parsed, Some(&current.config))?;
    std::fs::write(&path, raw)?;
    Ok(())
}

/// `colima start` flags that create a profile with `config`. Settings that
/// have no flag (daemon config, provision scripts) are saved to the file
/// after creation.
pub fn create_args(profile: &str, c: &ColimaConfig) -> Result<Vec<String>> {
    valid_profile(profile)?;
    validate(c, None)?;
    let mut a: Vec<String> = vec!["start".into(), "--profile".into(), profile.into()];
    let mut flag = |name: &str, v: String| {
        a.push(format!("--{name}={v}"));
    };
    flag("cpus", c.cpu.to_string());
    flag("memory", c.memory.to_string());
    flag("disk", c.disk.to_string());
    flag("root-disk", c.root_disk.to_string());
    if c.arch != "host" && !c.arch.is_empty() {
        flag("arch", c.arch.clone());
    }
    for (name, v) in [
        ("runtime", &c.runtime),
        ("vm-type", &c.vm_type),
        ("mount-type", &c.mount_type),
        ("cpu-type", &c.cpu_type),
        ("model-runner", &c.model_runner),
        ("hostname", &c.hostname),
        ("port-forwarder", &c.port_forwarder),
        ("disk-image", &c.disk_image),
        ("network-mode", &c.network.mode),
        ("network-interface", &c.network.interface),
        ("gateway-address", &c.network.gateway_address),
        ("kubernetes-version", &c.kubernetes.version),
    ] {
        if !v.trim().is_empty() {
            flag(name, v.trim().to_string());
        }
    }
    for (name, v) in [
        ("activate", c.auto_activate),
        ("binfmt", c.binfmt),
        ("vz-rosetta", c.rosetta),
        ("nested-virtualization", c.nested_virtualization),
        ("mount-inotify", c.mount_inotify),
        ("network-address", c.network.address),
        ("network-preferred-route", c.network.preferred_route),
        ("network-host-addresses", c.network.host_addresses),
        ("ssh-agent", c.forward_agent),
        ("ssh-config", c.ssh_config),
        ("kubernetes", c.kubernetes.enabled),
    ] {
        flag(name, v.to_string());
    }
    if c.kubernetes.port != 0 {
        flag("k3s-listen-port", c.kubernetes.port.to_string());
    }
    if c.ssh_port != 0 {
        flag("ssh-port", c.ssh_port.to_string());
    }
    for arg in &c.kubernetes.k3s_args {
        flag("k3s-arg", arg.clone());
    }
    for d in &c.network.dns {
        flag("dns", d.clone());
    }
    for (host, target) in &c.network.dns_hosts {
        flag("dns-host", format!("{host}={target}"));
    }
    for m in &c.mounts {
        let mut spec = m.location.clone();
        if let Some(mp) = m.mount_point.as_deref().filter(|s| !s.is_empty()) {
            spec.push(':');
            spec.push_str(mp);
        }
        if m.writable {
            spec.push_str(":w");
        }
        flag("mount", spec);
    }
    for (k, v) in &c.env {
        flag("env", format!("{k}={v}"));
    }
    Ok(a)
}

/// Whether creating with `config` needs a follow-up save for settings that
/// have no `colima start` flag.
pub fn needs_file_settings(c: &ColimaConfig) -> bool {
    !c.docker.is_empty() || !c.provision.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "cpu: 8\ndisk: 300\nmemory: 64\narch: aarch64\nruntime: docker\n\
        kubernetes:\n  enabled: false\n  version: v1.35.0+k3s1\n  k3sArgs:\n    - --disable=traefik\n  port: 0\n  futureKey: keep\n\
        network:\n  address: false\n  mode: shared\n  dns: null\n  dnsHosts: {}\n\
        docker: {}\nvmType: vz\nmountType: virtiofs\nprovision: null\nmounts: []\nenv: {}\nbrandNewSetting: 7\n";

    #[test]
    fn parses_nulls_and_defaults() {
        let c: ColimaConfig = serde_yaml::from_str(SAMPLE).unwrap();
        assert_eq!((c.cpu, c.disk, c.memory), (8, 300, 64.0));
        assert!(c.network.dns.is_empty());
        assert!(c.provision.is_empty());
        assert_eq!(c.kubernetes.k3s_args, ["--disable=traefik"]);
    }

    #[test]
    fn merge_keeps_unknown_keys() {
        let mut c: ColimaConfig = serde_yaml::from_str(SAMPLE).unwrap();
        c.cpu = 4;
        c.network.dns = vec!["1.1.1.1".into()];
        let Value::Mapping(mut base) = serde_yaml::from_str::<Value>(SAMPLE).unwrap() else {
            panic!()
        };
        let Value::Mapping(new) = serde_yaml::to_value(&c).unwrap() else {
            panic!()
        };
        merge(&mut base, new);
        let out = serde_yaml::to_string(&Value::Mapping(base)).unwrap();
        assert!(out.contains("brandNewSetting: 7"));
        assert!(out.contains("futureKey: keep"));
        assert!(out.contains("cpu: 4"));
        let back: ColimaConfig = serde_yaml::from_str(&out).unwrap();
        assert_eq!(back.network.dns, ["1.1.1.1"]);
    }

    #[test]
    fn validation_rules() {
        let cur: ColimaConfig = serde_yaml::from_str(SAMPLE).unwrap();
        let mut smaller = cur.clone();
        smaller.disk = 100;
        assert!(validate(&smaller, Some(&cur)).is_err());
        let mut arch = cur.clone();
        arch.arch = "x86_64".into();
        assert!(validate(&arch, Some(&cur)).is_err());
        assert!(validate(&cur, Some(&cur)).is_ok());
        assert!(config_path("../etc").is_err());
    }

    #[test]
    fn create_flags() {
        let mut c = ColimaConfig {
            cpu: 4,
            memory: 8.0,
            disk: 60,
            ..Default::default()
        };
        c.mounts.push(Mount {
            location: "~/src".into(),
            mount_point: None,
            writable: true,
        });
        let a = create_args("dev", &c).unwrap();
        assert_eq!(&a[..3], ["start", "--profile", "dev"]);
        assert!(a.contains(&"--cpus=4".to_string()));
        assert!(a.contains(&"--memory=8".to_string()));
        assert!(a.contains(&"--mount=~/src:w".to_string()));
        assert!(!a.iter().any(|x| x.starts_with("--arch")));
    }
}

#[cfg(test)]
mod live {
    /// Read-only: `cargo test colima_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn colima_live() {
        for p in ["default", "intel", "x64", "x86_64"] {
            match super::read(p) {
                Ok(f) => println!(
                    "{p}: cpu={} mem={} disk={} arch={} vm={} rt={:?} mounts={} k8s={}",
                    f.config.cpu,
                    f.config.memory,
                    f.config.disk,
                    f.config.arch,
                    f.config.vm_type,
                    f.config.runtime,
                    f.config.mounts.len(),
                    f.config.kubernetes.enabled
                ),
                Err(e) => println!("{p}: ERROR {e}"),
            }
        }
    }
}

#[cfg(test)]
mod roundtrip {
    /// Against a COPY: `COLIMA_HOME=<copy> cargo test colima_roundtrip -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn colima_roundtrip() {
        assert!(
            std::env::var_os("COLIMA_HOME").is_some(),
            "set COLIMA_HOME to a copy"
        );
        let before = super::read("default").unwrap();
        let mut c = before.config.clone();
        c.cpu = 6;
        c.network.dns = vec!["1.1.1.1".into()];
        c.docker
            .insert("insecure-registries".into(), serde_json::json!(["r:5000"]));
        super::write("default", &c).unwrap();
        let after = super::read("default").unwrap();
        assert_eq!(after.config, c);
        // Shrinking the disk is refused.
        c.disk -= 1;
        assert!(super::write("default", &c).is_err());
        println!("{}", after.raw);
    }
}

// ------------------------------------------------------------------ data

/// Lima instance name for a Colima profile (`default` → `colima`).
fn instance(profile: &str) -> String {
    if profile == "default" {
        "colima".into()
    } else {
        format!("colima-{profile}")
    }
}

/// Where Colima keeps a profile's VM and its container-data disk.
pub fn profile_paths(profile: &str) -> Result<(PathBuf, PathBuf)> {
    valid_profile(profile)?;
    let lima = colima_home()?.join("_lima");
    Ok((
        lima.join(instance(profile)),
        lima.join("_disks").join(instance(profile)),
    ))
}

/// Colima's download cache (`colima prune` clears it).
pub fn cache_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/colima")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
            .join("colima")
    })
}

#[cfg(test)]
mod data_tests {
    #[test]
    fn instance_names() {
        assert_eq!(super::instance("default"), "colima");
        assert_eq!(super::instance("x64"), "colima-x64");
        assert!(super::profile_paths("../x").is_err());
    }
}
