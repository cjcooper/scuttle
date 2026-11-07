# Container Desktop

A menu bar app for controlling local container runtimes: start/stop the VM, clean up images, and build images. Built with Rust + [Tauri 2](https://tauri.app).

macOS + Colima is the first target. The design also supports other Docker Desktop replacements on other operating systems.

## Features

- **Menu bar icon** (macOS) or system tray icon (Windows/Linux) showing live runtime status, with:
  - Start / Stop / Restart
  - **Open Shell ▸** any running container
  - **Shell User ▸** Image Default / root / My User (uid:gid) / your custom user
  - **Run Preset ▸** your saved `docker run` presets
  - Remove Dangling Images, Prune Build Cache, and Open Dashboard
- **Runtime**: pick a provider and profile, start, stop and restart it, and inspect every profile's CPU, memory and disk
- **Images ▸ Local**: pull; list, filter and multi-select remove; remove dangling or all unused images; prune the build cache; view disk usage. Each image has **Shell**, which runs `docker run -it --rm --user <shell user> --entrypoint <shell> IMAGE` in your terminal, and **Run…**, which prefills the Run form.
- **Images ▸ Build**: build context and Dockerfile pickers, tags, platform, target stage, build args, no-cache and pull options, with streamed BuildKit output
- **Containers ▸ Containers**: list and filter; **Shell** (`docker exec -it`), start, stop, restart and remove
- **Containers ▸ Run**: a full `docker run` form, or paste any `docker run …` command to fill it. Shows a live preview of the command, with copy. Runs interactively in a terminal window or detached, and can **save presets** that also appear in the menu bar.
- **ECR images**: when a Pull or Run image is an ECR URL (`<account>.dkr.ecr.<region>.amazonaws.com/…`), an **AWS profile** picker appears, along with **Sign in (SSO)** (`aws sso login`).
  - Docker then runs with `AWS_PROFILE`. With `docker-credential-ecr-login` configured, that alone is enough; otherwise the app logs in with `aws ecr get-login-password | docker login`.
  - The last profile used is remembered per registry, and presets keep their profile.
  - Pasting `AWS_PROFILE=x docker run …` fills the picker.
- **About** window (app menu, or the tray): version, maker, the MIT license, and every third-party license.

Interactive sessions (Shell, interactive Run, presets) open in **built-in shell windows**. Each is an [xterm.js](https://xtermjs.org) terminal (vendored in `ui/vendor/xterm`) backed by a real PTY that runs `docker exec -it` / `docker run -it`. The pattern is the same as k8s-subduer's exec windows:
- One window per session.
- The header has quick **Default / root / Me / Custom** user and shell switches. Changing either reconnects as that user.
- **Reconnect**.
- **Pop out ↗** reopens the same command in Terminal/iTerm2.
- Typing `exit` closes the window. Failures stay on screen so you can read them.
- Closing the window of a throwaway (`--rm`) run force-removes its container.

At startup the app copies your login shell's environment, so `AWS_PROFILE` and credential helpers such as `docker-credential-ecr-login` work the same way they do from your own prompt.

While any window is open (dashboard, a shell, About), the app has a Dock icon and a full menu bar menu. Closing the last window returns it to a menu-bar-only app. Quit from the app menu or the tray.

A shell window's user and shell choices become the defaults for the next shell, and for the tray's **Shell User ▸** menu.

## Requirements

- Rust (stable) and the Tauri CLI: `cargo install tauri-cli --version "^2"`
- The `docker` CLI, used as the client for every engine
- A runtime, such as Colima (`brew install colima docker`)

## Run

```sh
cd src-tauri
cargo tauri dev         # run in development
cargo tauri build       # produce a .app / .dmg bundle
cargo test              # unit tests
cargo test -- --ignored --nocapture   # live smoke test against local Colima
```

The UI is plain HTML, CSS and JS in `ui/`. It uses Tauri's global API, so there is no Node build step.

## Architecture

```
src-tauri/src/
  process.rs        spawn CLIs with an augmented PATH (GUI apps don't get Homebrew's PATH)
  engine.rs         Docker-API operations via the docker CLI, targeted by DOCKER_HOST or --context
  runtime/
    mod.rs          Provider trait: profiles, start/stop/restart, engine_target
    colima.rs       Colima profiles (colima list/status/start/stop)
    podman.rs       podman machine (macOS/Windows/Linux)
    docker_context.rs  any docker context: Docker Desktop, OrbStack, Rancher Desktop, remote hosts
  aws.rs            ECR detection, AWS profiles, ECR login / SSO sign-in
  menu.rs           app menu (with About) and Dock-icon switching
  about.rs          About window (license + third-party notices)
  state.rs          selected provider/profile (persisted), busy tracking, status
  commands.rs       Tauri IPC commands + actions shared with the tray
  shell.rs          integrated shell windows: PTY (portable-pty) running docker exec/run -it, streamed to xterm.js
  terminal.rs       "Pop out" to Terminal / iTerm2 / platform terminal
  tray.rs           menu bar icon, dynamic menu (shells, presets, user switch), 10s poller
ui/                 dashboard (index.html, app.js, style.css), shell window (shell.html, shell.js)
```

Each runtime exposes a Docker-compatible API socket, so a provider only has to say where the engine is (`EngineTarget`) and, optionally, how to start and stop it. To add a runtime such as Rancher Desktop's `rdctl` or WSL, implement `runtime::Provider` and register it in `runtime::all()`.

The selection is saved to the app config directory as `settings.json`. On macOS that is `~/Library/Application Support/com.it-problems.container-desktop/`.

## Next steps

- Colima start options (CPU, memory, disk, arch, VM type) and creating or deleting profiles
- Cancelling builds, plus a build history
- Volume and network cleanup (`system prune`)
- Rancher Desktop (`rdctl`) and Docker Desktop (`docker desktop start/stop`) lifecycle providers
- containerd/nerdctl support for Colima profiles that use the containerd runtime

## Third-party notices

`THIRD-PARTY-NOTICES.txt` is compiled into the About window. Regenerate it after changing dependencies:

```sh
cargo install cargo-about --features cli   # once
tools/third-party-notices.sh
```

## License

MIT, see [LICENSE](LICENSE). Copyright (c) 2026 it-problems.

Bundled third-party code: [xterm.js](https://github.com/xtermjs/xterm.js) and `@xterm/addon-fit` (MIT), in `ui/vendor/xterm` with their license.
