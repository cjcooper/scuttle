//! AWS ECR credentials via `docker-credential-ecr-login`.
//!
//! When docker's config routes a registry to the `ecr-login` credential
//! helper (`credHelpers` or `credsStore` in `~/.docker/config.json`), the
//! helper picks AWS credentials from `AWS_PROFILE`. So for those registries
//! the app asks for a profile, runs docker with `AWS_PROFILE` set, and checks
//! up front that the helper can get credentials. Docker itself hides helper
//! failures behind "no basic auth credentials".

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::error::{AppError, Result};
use crate::process::{self, LineSink};

/// Prefix of the error for a profile that can't get credentials; the UI
/// matches it to offer SSO sign-in.
pub const SIGN_IN_REQUIRED: &str = "AWS sign-in required for profile";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EcrRegistry {
    /// e.g. `123456789012.dkr.ecr.us-east-1.amazonaws.com` or `public.ecr.aws`
    pub host: String,
    /// For private ECR registries.
    pub account: Option<String>,
    pub region: Option<String>,
}

/// The registry host of an image reference (`docker.io` when none is given),
/// following docker's rule: the first path component is a registry if it
/// contains `.` or `:`, or is `localhost`.
pub fn registry_of(image: &str) -> String {
    let image = image.trim();
    match image.split_once('/') {
        Some((first, _)) if first.contains(['.', ':']) || first == "localhost" => {
            first.to_lowercase()
        }
        _ => "docker.io".into(),
    }
}

/// `<account>.dkr.ecr[-fips].<region>.amazonaws.com[.cn]` → (account, region).
fn ecr_parts(host: &str) -> Option<(String, String)> {
    let mut parts = host.split('.');
    let account = parts.next()?;
    if account.len() != 12 || !account.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if parts.next()? != "dkr" || !parts.next()?.starts_with("ecr") {
        return None;
    }
    let region = parts.next()?;
    let tail: Vec<&str> = parts.collect();
    (tail == ["amazonaws", "com"] || tail == ["amazonaws", "com", "cn"])
        .then(|| (account.into(), region.into()))
}

fn docker_config() -> Option<serde_json::Value> {
    let dir = std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".docker")))?;
    serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).ok()?).ok()
}

/// The credential helper docker uses for `host` (credHelpers, else credsStore).
fn helper_in(config: &serde_json::Value, host: &str) -> Option<String> {
    config["credHelpers"][host]
        .as_str()
        .or_else(|| config["credsStore"].as_str())
        .map(str::to_string)
}

/// The registry `image` comes from, when docker is configured to use the
/// `ecr-login` helper for it.
pub fn ecr_registry(image: &str) -> Option<EcrRegistry> {
    if image.trim().is_empty() {
        return None;
    }
    let host = registry_of(image);
    if helper_in(&docker_config()?, &host).as_deref() != Some("ecr-login") {
        return None;
    }
    let (account, region) = ecr_parts(&host).unzip();
    Some(EcrRegistry {
        host,
        account,
        region,
    })
}

fn aws_file(env: &str, name: &str) -> Option<PathBuf> {
    std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aws").join(name)))
}

/// Section names from an AWS ini file. In `config` they are `[profile x]`
/// (or `[default]`); in `credentials` just `[x]`. `[sso-session x]` and other
/// non-profile sections are skipped.
fn ini_profiles(text: &str, config: bool) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix('[')?.strip_suffix(']'))
        .map(str::trim)
        .filter_map(|s| {
            if !config || s == "default" {
                Some(s.to_string())
            } else {
                s.strip_prefix("profile ").map(|p| p.trim().to_string())
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// All profiles from `~/.aws/config` and `~/.aws/credentials`.
pub fn profiles() -> Vec<String> {
    let mut set = BTreeSet::new();
    for (env, name, config) in [
        ("AWS_CONFIG_FILE", "config", true),
        ("AWS_SHARED_CREDENTIALS_FILE", "credentials", false),
    ] {
        if let Some(text) = aws_file(env, name).and_then(|p| std::fs::read_to_string(p).ok()) {
            set.extend(ini_profiles(&text, config));
        }
    }
    set.into_iter().collect()
}

/// Ask `docker-credential-ecr-login` for `host`'s credentials as `profile`.
/// The credentials themselves are discarded.
async fn helper_ok(host: &str, profile: &str) -> Result<bool> {
    let mut cmd = process::command("docker-credential-ecr-login")?;
    cmd.arg("get")
        .env("AWS_PROFILE", profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(host.as_bytes()).await?;
    }
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .map_err(|_| AppError::msg("docker-credential-ecr-login timed out"))??;
    Ok(status.success())
}

/// A readable reason why `profile` has no credentials, from the AWS CLI.
async fn credentials_problem(profile: &str) -> String {
    let Ok(mut cmd) = process::command("aws") else {
        return "the ECR credential helper could not get credentials".into();
    };
    cmd.args(["sts", "get-caller-identity", "--profile", profile]);
    match process::output(cmd, Duration::from_secs(20)).await {
        // STS works, so the profile lacks ECR access to this registry.
        Ok(_) => "credentials work, but the ECR credential helper was refused (check this profile's access to the registry)".into(),
        Err(e) => {
            let msg = e.to_string();
            let msg = msg.split_once(": ").map(|(_, m)| m).unwrap_or(&msg).trim();
            if msg.is_empty() {
                "the ECR credential helper could not get credentials".into()
            } else {
                msg.to_string()
            }
        }
    }
}

/// Make sure the ECR helper can authenticate to `registry` as `profile`
/// (docker must then run with `AWS_PROFILE=profile`). On failure the error
/// starts with [`SIGN_IN_REQUIRED`].
pub async fn check_credentials(
    registry: &EcrRegistry,
    profile: &str,
    sink: &LineSink,
) -> Result<()> {
    sink(&format!(
        "Checking AWS credentials for {} with profile `{profile}`…",
        registry.host
    ));
    if helper_ok(&registry.host, profile).await? {
        return Ok(());
    }
    let why = credentials_problem(profile).await;
    sink(&format!("AWS credentials for `{profile}` failed: {why}"));
    Err(AppError::msg(format!(
        "{SIGN_IN_REQUIRED} `{profile}`: {why}"
    )))
}

/// `aws sso login --profile <profile>` (opens the browser).
pub async fn sso_login(profile: &str, sink: LineSink) -> Result<()> {
    let mut cmd = process::command("aws")?;
    cmd.args(["sso", "login", "--profile", profile]);
    sink(&format!("$ aws sso login --profile {profile}"));
    process::stream(cmd, sink).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registries() {
        assert_eq!(
            registry_of("396608815702.dkr.ecr.us-east-1.amazonaws.com/php:APP-1"),
            "396608815702.dkr.ecr.us-east-1.amazonaws.com"
        );
        assert_eq!(registry_of("public.ecr.aws/nginx/nginx"), "public.ecr.aws");
        assert_eq!(registry_of("localhost:5000/x"), "localhost:5000");
        assert_eq!(registry_of("nginx:latest"), "docker.io");
        assert_eq!(registry_of("library/nginx"), "docker.io");
    }

    #[test]
    fn ecr_hosts() {
        assert_eq!(
            ecr_parts("396608815702.dkr.ecr.us-east-1.amazonaws.com"),
            Some(("396608815702".into(), "us-east-1".into()))
        );
        assert!(ecr_parts("123456789012.dkr.ecr-fips.us-gov-west-1.amazonaws.com").is_some());
        assert!(ecr_parts("123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn").is_some());
        assert!(ecr_parts("public.ecr.aws").is_none());
    }

    #[test]
    fn helper_lookup() {
        let cfg = json!({"credHelpers": {"a.example": "ecr-login"}, "credsStore": "osxkeychain"});
        assert_eq!(helper_in(&cfg, "a.example").as_deref(), Some("ecr-login"));
        assert_eq!(helper_in(&cfg, "b.example").as_deref(), Some("osxkeychain"));
        assert_eq!(helper_in(&json!({}), "a.example"), None);
    }

    #[test]
    fn parses_profiles() {
        let config =
            "[default]\nregion=us-east-1\n[profile dev]\n[sso-session corp]\n[ profile prod ]\n";
        assert_eq!(ini_profiles(config, true), ["default", "dev", "prod"]);
        assert_eq!(ini_profiles("[default]\n[ci]\n", false), ["default", "ci"]);
    }
}

#[cfg(test)]
mod live {
    /// `cargo test aws_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn aws_live() {
        let p = super::profiles();
        println!("{} profiles", p.len());
        println!(
            "{:?}",
            super::ecr_registry("396608815702.dkr.ecr.us-east-1.amazonaws.com/php:x")
        );
        println!("{:?}", super::ecr_registry("nginx"));
    }
}
