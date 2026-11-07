//! AWS ECR support: recognising ECR image references, listing the user's AWS
//! profiles, and making sure docker can authenticate as a chosen profile.
//!
//! With `docker-credential-ecr-login` configured (credHelpers / credsStore),
//! setting `AWS_PROFILE` on the docker process is enough. Otherwise we log in
//! explicitly with `aws ecr get-login-password | docker login`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::engine::Docker;
use crate::error::{AppError, Result};
use crate::process::{self, LineSink};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EcrRegistry {
    /// e.g. `123456789012.dkr.ecr.us-east-1.amazonaws.com`
    pub host: String,
    pub account: String,
    pub region: String,
}

/// The ECR registry an image reference points at, if any.
pub fn ecr_registry(image: &str) -> Option<EcrRegistry> {
    let image = image.trim();
    let (host, rest) = image.split_once('/')?;
    if rest.is_empty() {
        return None;
    }
    // <account>.dkr.ecr[-fips].<region>.amazonaws.com[.cn]
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
    if !(tail == ["amazonaws", "com"] || tail == ["amazonaws", "com", "cn"]) || region.is_empty() {
        return None;
    }
    Some(EcrRegistry {
        host: host.to_lowercase(),
        account: account.into(),
        region: region.into(),
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

/// Whether docker is configured to use `docker-credential-ecr-login` for
/// `host` (so `AWS_PROFILE` alone is enough).
fn uses_ecr_helper(host: &str) -> bool {
    let dir = std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".docker")));
    let Some(text) = dir.and_then(|d| std::fs::read_to_string(d.join("config.json")).ok()) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    v["credHelpers"][host].as_str() == Some("ecr-login")
        || v["credsStore"].as_str() == Some("ecr-login")
}

/// Make sure `docker` can pull from `registry` as `profile`. Logs progress
/// to `sink`. The caller must also run docker with `AWS_PROFILE=profile`.
pub async fn ensure_login(
    docker: &Docker,
    registry: &EcrRegistry,
    profile: &str,
    sink: &LineSink,
) -> Result<()> {
    if uses_ecr_helper(&registry.host) {
        sink(&format!(
            "Using docker-credential-ecr-login with AWS profile `{profile}`"
        ));
        return Ok(());
    }
    sink(&format!(
        "Logging in to {} with AWS profile `{profile}`…",
        registry.host
    ));
    let mut aws = process::command("aws")?;
    aws.args([
        "ecr",
        "get-login-password",
        "--region",
        &registry.region,
        "--profile",
        profile,
    ]);
    let password = process::output(aws, process::QUICK).await.map_err(|e| {
        AppError::msg(format!(
            "aws ecr get-login-password failed ({e}). If this profile uses SSO, sign in first."
        ))
    })?;

    let mut login = docker.cli(&[
        "login",
        "--username",
        "AWS",
        "--password-stdin",
        &registry.host,
    ])?;
    login
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = login.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(password.trim().as_bytes()).await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        return Err(AppError::msg(format!(
            "docker login failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    sink("Login Succeeded");
    Ok(())
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

    #[test]
    fn recognises_ecr_images() {
        let r = ecr_registry(
            "396608815702.dkr.ecr.us-east-1.amazonaws.com/php:APP-1.20261021.39600767925",
        )
        .unwrap();
        assert_eq!(r.account, "396608815702");
        assert_eq!(r.region, "us-east-1");
        assert_eq!(r.host, "396608815702.dkr.ecr.us-east-1.amazonaws.com");
        assert!(ecr_registry("123456789012.dkr.ecr-fips.us-gov-west-1.amazonaws.com/x").is_some());
        assert!(ecr_registry("123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn/x").is_some());
        assert!(ecr_registry("public.ecr.aws/nginx/nginx").is_none());
        assert!(ecr_registry("nginx:latest").is_none());
        assert!(ecr_registry("123.dkr.ecr.us-east-1.amazonaws.com/x").is_none());
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
        println!("{} profiles: {:?}", p.len(), p);
        println!("helper for 396608815702: {}", super::uses_ecr_helper("396608815702.dkr.ecr.us-east-1.amazonaws.com"));
    }
}
