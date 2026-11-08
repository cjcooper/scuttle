//! Docker-compatible engine operations, driven through the `docker` CLI.
//!
//! Every runtime (Colima, Podman machine, Docker Desktop, OrbStack, Rancher
//! Desktop, ...) exposes a Docker-API socket, so a single client works for all
//! of them. A provider only needs to say *where* the engine is via
//! [`EngineTarget`].

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::{AppError, Result};
use crate::process::{self, LineSink};

/// How to reach a Docker-API endpoint.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineTarget {
    /// Explicit `DOCKER_HOST`, e.g. `unix:///Users/me/.colima/default/docker.sock`.
    pub host: Option<String>,
    /// A named `docker context`.
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "PascalCase"))]
pub struct ImageInfo {
    #[serde(rename(deserialize = "ID"))]
    pub id: String,
    pub repository: String,
    pub tag: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub created_since: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub containers: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "PascalCase"))]
pub struct ContainerInfo {
    #[serde(rename(deserialize = "ID"))]
    pub id: String,
    pub names: String,
    pub image: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub ports: String,
    #[serde(default)]
    pub running_for: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "PascalCase"))]
pub struct DiskUsage {
    #[serde(rename(deserialize = "Type"))]
    pub kind: String,
    #[serde(default)]
    pub total_count: String,
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub reclaimable: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRequest {
    /// Build context directory.
    pub context: String,
    /// Dockerfile path; relative paths are resolved by docker against the CWD,
    /// so the UI sends absolute paths.
    pub dockerfile: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub platform: Option<String>,
    pub target: Option<String>,
    #[serde(default)]
    pub build_args: Vec<String>,
    #[serde(default)]
    pub no_cache: bool,
    #[serde(default)]
    pub pull: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Remove,
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

pub struct Docker {
    target: EngineTarget,
    /// Extra environment for docker processes (e.g. `AWS_PROFILE` for ECR
    /// credential helpers).
    extra_env: Vec<(String, String)>,
}

impl Docker {
    pub fn new(target: EngineTarget) -> Self {
        Self {
            target,
            extra_env: Vec::new(),
        }
    }

    /// Run docker with `AWS_PROFILE` set (for ECR credential helpers).
    pub fn with_aws_profile(mut self, profile: Option<&str>) -> Self {
        if let Some(p) = profile.map(str::trim).filter(|p| !p.is_empty()) {
            self.extra_env.push(("AWS_PROFILE".into(), p.into()));
        }
        self
    }

    /// Global CLI args that select the target (`--context NAME`).
    pub fn global_args(&self) -> Vec<String> {
        match (&self.target.host, &self.target.context) {
            (None, Some(ctx)) => vec!["--context".into(), ctx.clone()],
            _ => Vec::new(),
        }
    }

    /// Environment for docker processes: the target (`DOCKER_HOST`) plus
    /// any extras such as `AWS_PROFILE`.
    pub fn env(&self) -> Vec<(String, String)> {
        self.target
            .host
            .iter()
            .map(|h| ("DOCKER_HOST".to_string(), h.clone()))
            .chain(self.extra_env.iter().cloned())
            .collect()
    }

    /// A docker command against this engine, for callers that need their
    /// own working directory, environment or stdio.
    pub fn cli(&self, args: &[&str]) -> Result<tokio::process::Command> {
        self.command(args)
    }

    /// Whether `image` is present locally (so running it won't pull).
    pub async fn image_exists(&self, image: &str) -> bool {
        self.run(
            &["image", "inspect", "--format", "{{.Id}}", image],
            Duration::from_secs(10),
        )
        .await
        .is_ok()
    }

    fn command(&self, args: &[&str]) -> Result<tokio::process::Command> {
        let mut cmd = process::command("docker")?;
        // Never let an inherited environment override the selected target.
        cmd.env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
        cmd.envs(self.env());
        cmd.args(self.global_args());
        cmd.args(args);
        Ok(cmd)
    }

    pub async fn force_remove(&self, container: &str) -> Result<()> {
        self.run(
            &["container", "rm", "--force", container],
            Duration::from_secs(30),
        )
        .await
        .map(|_| ())
    }

    async fn run(&self, args: &[&str], timeout: Duration) -> Result<String> {
        process::output(self.command(args)?, timeout).await
    }

    async fn json_lines<T: DeserializeOwned>(&self, args: &[&str]) -> Result<Vec<T>> {
        let out = self.run(args, process::QUICK).await?;
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).map_err(AppError::from))
            .collect()
    }

    pub async fn server_version(&self) -> Result<String> {
        let v = self
            .run(
                &["version", "--format", "{{.Server.Version}}"],
                Duration::from_secs(5),
            )
            .await?;
        Ok(v.trim().to_string())
    }

    pub async fn images(&self) -> Result<Vec<ImageInfo>> {
        self.json_lines(&["image", "ls", "--format", "{{json .}}"])
            .await
    }

    pub async fn containers(&self) -> Result<Vec<ContainerInfo>> {
        self.json_lines(&["container", "ls", "--all", "--format", "{{json .}}"])
            .await
    }

    pub async fn disk_usage(&self) -> Result<Vec<DiskUsage>> {
        self.json_lines(&["system", "df", "--format", "{{json .}}"])
            .await
    }

    pub async fn remove_images(&self, ids: &[String], force: bool) -> Result<String> {
        if ids.is_empty() {
            return Ok(String::new());
        }
        let mut args = vec!["image", "rm"];
        if force {
            args.push("--force");
        }
        args.extend(ids.iter().map(String::as_str));
        self.run(&args, Duration::from_secs(120)).await
    }

    /// `all = false` removes dangling images only; `true` removes every image
    /// not used by a container.
    pub async fn prune_images(&self, all: bool) -> Result<String> {
        let mut args = vec!["image", "prune", "--force"];
        if all {
            args.push("--all");
        }
        self.run(&args, Duration::from_secs(300)).await
    }

    pub async fn prune_build_cache(&self, all: bool) -> Result<String> {
        let mut args = vec!["builder", "prune", "--force"];
        if all {
            args.push("--all");
        }
        self.run(&args, Duration::from_secs(300)).await
    }

    pub async fn container_action(&self, id: &str, action: ContainerAction) -> Result<()> {
        let args: &[&str] = match action {
            ContainerAction::Start => &["container", "start", id],
            ContainerAction::Stop => &["container", "stop", id],
            ContainerAction::Restart => &["container", "restart", id],
            ContainerAction::Remove => &["container", "rm", "--force", id],
        };
        self.run(args, Duration::from_secs(60)).await.map(|_| ())
    }

    pub async fn build(&self, req: &BuildRequest, sink: LineSink) -> Result<()> {
        let mut args: Vec<String> = vec!["build".into(), "--progress=plain".into()];
        if let Some(f) = non_empty(&req.dockerfile) {
            args.extend(["--file".into(), f.into()]);
        }
        for t in req.tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            args.extend(["--tag".into(), t.into()]);
        }
        if let Some(p) = non_empty(&req.platform) {
            args.extend(["--platform".into(), p.into()]);
        }
        if let Some(t) = non_empty(&req.target) {
            args.extend(["--target".into(), t.into()]);
        }
        for a in req
            .build_args
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
        {
            args.extend(["--build-arg".into(), a.into()]);
        }
        if req.no_cache {
            args.push("--no-cache".into());
        }
        if req.pull {
            args.push("--pull".into());
        }
        args.push(req.context.clone());

        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        sink(&format!("$ docker {}", argv.join(" ")));
        let mut cmd = self.command(&argv)?;
        cmd.current_dir(&req.context);
        process::stream(cmd, sink).await
    }

    pub async fn pull(&self, image: &str, sink: LineSink) -> Result<()> {
        sink(&format!("$ docker pull {image}"));
        process::stream(self.command(&["pull", image])?, sink).await
    }

    /// Start a detached container; returns the new container id.
    pub async fn run_detached(&self, req: &RunRequest) -> Result<String> {
        let args = run_args(req)?;
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(self
            .run(&argv, Duration::from_secs(600))
            .await?
            .trim()
            .to_string())
    }

    /// A full shell command line (binary, target and args, quoted) for
    /// running docker in an external terminal.
    pub fn command_line(&self, args: &[String]) -> Result<String> {
        let bin =
            process::resolve("docker").ok_or_else(|| AppError::msg("`docker` was not found"))?;
        let mut words: Vec<String> = self
            .env()
            .iter()
            .map(|(k, v)| format!("{k}={}", shell_quote(v)))
            .collect();
        words.push(shell_quote(&bin.to_string_lossy()));
        if self.target.host.is_none() {
            if let Some(ctx) = &self.target.context {
                words.push("--context".into());
                words.push(shell_quote(ctx));
            }
        }
        words.extend(args.iter().map(|a| shell_quote(a)));
        Ok(words.join(" "))
    }
}

// ------------------------------------------------------------ run / exec

/// Options for `docker container run`, shared by the Run form, presets and
/// the quick "shell into image" action.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunRequest {
    pub image: String,
    pub name: Option<String>,
    /// `""` = image default, `"host"` = the host user's uid:gid, anything
    /// else is passed to `--user` verbatim (e.g. `root`, `1000:1000`).
    pub user: String,
    pub entrypoint: Option<String>,
    pub command: Vec<String>,
    pub ports: Vec<String>,
    pub env: Vec<String>,
    pub volumes: Vec<String>,
    pub workdir: Option<String>,
    pub platform: Option<String>,
    pub network: Option<String>,
    /// `-it` in a terminal window when true, otherwise `--detach`.
    pub interactive: bool,
    /// `--rm`
    pub remove: bool,
    /// Extra raw arguments inserted before the image.
    pub extra_args: Vec<String>,
    /// AWS profile for pulling from ECR (sets `AWS_PROFILE`, logs in if needed).
    pub aws_profile: Option<String>,
}

/// Run `sh -c` with this to get bash when the image has it, else sh.
pub const AUTO_SHELL: &str =
    "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi";

/// Resolve a user spec into a `--user` value.
pub fn resolve_user(spec: &str) -> Result<Option<String>> {
    match spec.trim() {
        "" | "default" => Ok(None),
        "host" => host_user().map(Some),
        other => Ok(Some(other.to_string())),
    }
}

#[cfg(unix)]
fn host_user() -> Result<String> {
    let id = |flag: &str| -> Result<String> {
        let out = std::process::Command::new("id").arg(flag).output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    Ok(format!("{}:{}", id("-u")?, id("-g")?))
}

#[cfg(not(unix))]
fn host_user() -> Result<String> {
    Err(AppError::msg(
        "the host user option is only available on macOS and Linux",
    ))
}

/// `[entrypoint override, argv]` for an interactive shell. `"auto"`/empty
/// picks bash when present.
pub fn shell_argv(shell: &str) -> Vec<String> {
    match shell.trim() {
        "" | "auto" => vec!["sh".into(), "-c".into(), AUTO_SHELL.into()],
        s => vec![s.into()],
    }
}

/// A throwaway interactive container whose entrypoint is a shell, like
/// `docker run -it --rm --user root --entrypoint /bin/bash IMAGE`.
pub fn shell_run_request(image: &str, user: &str, shell: &str) -> RunRequest {
    let mut argv = shell_argv(shell).into_iter();
    RunRequest {
        image: image.into(),
        user: user.into(),
        entrypoint: argv.next(),
        command: argv.collect(),
        interactive: true,
        remove: true,
        ..Default::default()
    }
}

pub fn run_args(req: &RunRequest) -> Result<Vec<String>> {
    let image = req.image.trim();
    if image.is_empty() {
        return Err(AppError::msg("an image is required"));
    }
    let mut a: Vec<String> = vec!["container".into(), "run".into()];
    a.push(if req.interactive { "-it" } else { "--detach" }.into());
    if req.remove {
        a.push("--rm".into());
    }
    let mut opt = |flag: &str, v: &str| {
        let v = v.trim();
        if !v.is_empty() {
            a.push(flag.into());
            a.push(v.into());
        }
    };
    if let Some(n) = &req.name {
        opt("--name", n);
    }
    if let Some(u) = resolve_user(&req.user)? {
        opt("--user", &u);
    }
    if let Some(e) = &req.entrypoint {
        opt("--entrypoint", e);
    }
    if let Some(w) = &req.workdir {
        opt("--workdir", w);
    }
    if let Some(p) = &req.platform {
        opt("--platform", p);
    }
    if let Some(n) = &req.network {
        opt("--network", n);
    }
    for p in &req.ports {
        opt("--publish", p);
    }
    for e in &req.env {
        opt("--env", e);
    }
    for v in &req.volumes {
        opt("--volume", v);
    }
    a.extend(
        req.extra_args
            .iter()
            .filter(|s| !s.trim().is_empty())
            .cloned(),
    );
    a.push(image.into());
    a.extend(req.command.iter().cloned());
    Ok(a)
}

pub fn exec_args(container: &str, user: &str, shell: &str) -> Result<Vec<String>> {
    let mut a: Vec<String> = vec!["container".into(), "exec".into(), "-it".into()];
    if let Some(u) = resolve_user(user)? {
        a.push("--user".into());
        a.push(u);
    }
    a.push(container.into());
    a.extend(shell_argv(shell));
    Ok(a)
}

/// Quote a word for POSIX sh (or cmd.exe on Windows).
pub fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        if s.is_empty() || s.contains([' ', '\t', '"', '&', '|', '<', '>', '^']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    } else if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_cli_json() {
        let img: ImageInfo = serde_json::from_str(r#"{"Containers":"1","CreatedAt":"2026-11-07 14:59:20 -0500 EST","CreatedSince":"2 hours ago","Digest":"<none>","ID":"d0788a934446","Repository":"remoter-rdptest","SharedSize":"N/A","Size":"836MB","Tag":"latest","UniqueSize":"N/A"}"#).unwrap();
        assert_eq!(
            (img.id.as_str(), img.size.as_str()),
            ("d0788a934446", "836MB")
        );

        let c: ContainerInfo = serde_json::from_str(r#"{"Command":"\"/start.sh\"","ID":"38809c71a3fd","Image":"e9506996b935","Names":"remoter-rdptest","Platform":{"architecture":"arm64","os":"linux"},"Ports":"0.0.0.0:13389->3389/tcp","RunningFor":"2 hours ago","State":"running","Status":"Up 2 hours"}"#).unwrap();
        assert_eq!(c.state, "running");

        let d: DiskUsage = serde_json::from_str(r#"{"Active":"4","Reclaimable":"2.474GB (44%)","Size":"5.587GB","TotalCount":"9","Type":"Images"}"#).unwrap();
        assert_eq!(d.kind, "Images");

        // Serialized for the UI in camelCase.
        let v = serde_json::to_value(&img).unwrap();
        assert_eq!(v["createdSince"], "2 hours ago");
    }

    #[test]
    fn builds_image_shell_command() {
        let req = shell_run_request("example/php:APP-1", "root", "/bin/bash");
        assert_eq!(
            run_args(&req).unwrap().join(" "),
            "container run -it --rm --user root --entrypoint /bin/bash example/php:APP-1"
        );
        let auto = shell_run_request("alpine", "", "auto");
        let args = run_args(&auto).unwrap();
        assert_eq!(&args[2..6], ["-it", "--rm", "--entrypoint", "sh"]);
        assert_eq!(args[6], "alpine");
        assert_eq!(args[7], "-c");
    }

    #[test]
    fn exec_and_quoting() {
        assert_eq!(
            exec_args("web", "root", "/bin/bash").unwrap().join(" "),
            "container exec -it --user root web /bin/bash"
        );
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(
            shell_quote("unix:///x/docker.sock"),
            "unix:///x/docker.sock"
        );
    }

    /// Live smoke test against the local engine: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_engine() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            use crate::runtime::Provider;
            let p = crate::runtime::Colima;
            let profiles = p.profiles().await.unwrap();
            println!("profiles: {profiles:#?}");
            let d = Docker::new(p.engine_target("default").await.unwrap());
            println!("server {}", d.server_version().await.unwrap());
            println!("{} images", d.images().await.unwrap().len());
            println!("{} containers", d.containers().await.unwrap().len());
            println!("{:?}", d.disk_usage().await.unwrap());
            let ctx = crate::runtime::DockerContext.profiles().await.unwrap();
            println!(
                "contexts: {:?}",
                ctx.iter().map(|c| &c.name).collect::<Vec<_>>()
            );
        });
    }
}
