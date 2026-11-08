# Container Desktop

A menu bar app for controlling local container runtimes: start/stop the VM, clean up images, and build images. Built with Rust + [Tauri 2](https://tauri.app).

macOS + Colima is the first target. The design also supports other Docker Desktop replacements on other operating systems.

## Features

- **Menu bar icon** (macOS) or system tray icon (Windows/Linux). A status badge shows the runtime state: ● running, ○ stopped, half-filled while starting or stopping, ⊖ when the engine is unreachable. The menu has:
  - Start / Stop / Restart
  - **Open Shell ▸** any running container
  - **Shell User ▸** Image Default / root / My User (uid:gid) / your custom user
  - **Run Preset ▸** your saved `docker run` presets
  - Remove Dangling Images, Prune Build Cache, and Open Dashboard
- **Runtime**: pick a provider and profile, start, stop and restart it, and inspect every profile's CPU, memory and disk
- **Images ▸ Local**: pull; list, filter and multi-select remove; remove dangling or all unused images; prune the build cache; view disk usage. Each image has **Shell**, which runs `docker run -it --rm --user <shell user> --entrypoint <shell> IMAGE` in your terminal, and **Run…**, which prefills the Run form.
- **Images ▸ Build**: build context and Dockerfile pickers, tags, platform, target stage, build args, no-cache and pull options, with streamed BuildKit output
- **History**: every run, pull and shell the app starts, from the dashboard or the menu bar, with the exact docker command (including `AWS_PROFILE` for ECR).
  - Filter by text or kind; failed commands are marked.
  - **Run again**, **Edit** (opens it in the Run form), **Open shell**, **Pull**, **Copy** and remove.
  - Up to 500 entries, saved in `history.json` next to `settings.json`.
- **Pull without running**: **Pull image** in the Run form, **Pull** on each tagged image (to refresh it), and **Pull** on history entries. These pulls use the same AWS profile and sign-in handling as the Images ▸ Local Pull box.
- **Containers ▸ Containers**: list and filter; **Shell** (`docker exec -it`), start, stop, restart and remove
- **Containers ▸ Run**: a full `docker run` form, or paste any `docker run …` command to fill it. Shows a live preview of the command, with copy. Runs interactively in a terminal window or detached, and can **save presets** that also appear in the menu bar.
- **Colima settings** (Runtime ▸ **Configure…**, or **Configure** on a profile row, when the provider is Colima): a dialog to edit any profile's `colima.yaml`. You can edit it with a form, or directly in the **YAML** tab. **Save & Restart** applies the changes.
  - **Resources**: CPU and memory sliders capped at your Mac's limits; disk, which can only grow; root disk.
  - **Virtual machine**: arch, VM type, runtime and mount type (these four can only be set when a profile is created), CPU type, hostname, model runner, Rosetta, binfmt, nested virtualization, inotify.
  - **Kubernetes**: enable, version, k3s args, API port.
  - **Network**: shared or bridged mode, reachable IP, gateway, DNS resolvers and DNS hosts.
  - **Mounts**, VM **environment**, **Docker daemon.json**, **provision scripts**, and SSH and port-forwarder options.
  - Settings the form doesn't know about are kept when saving.
  - **New profile…** creates and starts a VM with the chosen settings. **Delete profile…** can optionally delete its container data too.
  - **Data** tab:
    - disk used by each profile's VM and container data, with **Delete with data** (`colima delete --data`);
    - **Clear download cache** (`colima prune --all`);
    - **Delete everything**: every profile and its data, behind a typed `DELETE ALL` confirmation.
- **ECR images**: when `~/.docker/config.json` routes an image's registry to `docker-credential-ecr-login` (`credHelpers` or `credsStore`), Pull and Run show an **AWS profile** picker and docker runs with `AWS_PROFILE` set.
  - Before pulling, the app asks the helper for credentials. An expired SSO session then shows "AWS sign-in required for profile …" with **Sign in (SSO)** (`aws sso login`) and a retry, instead of docker's "no basic auth credentials". This works in shell windows too.
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

**Open at login** (Runtime ▸ Startup, or **Open at Login** in the menu bar) registers the app with the OS: a LaunchAgent on macOS, the Run registry key on Windows, an autostart `.desktop` file on Linux. When opened at login, the app stays in the menu bar and starts the selected Colima or Podman profile if it isn't running. Starting the profile can be switched off separately. Only one copy of the app runs at a time; opening it again shows the dashboard.

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
  login.rs          open at login (tauri-plugin-autostart) and starting the provider then
  history.rs        command history (history.json)
  colima_config.rs  read/validate/merge colima.yaml, create-profile flags
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

## App icon

The icon comes in light and dark versions in the same family and palette as Subduer: a container in a control dial.
- **Light** is the default everywhere: Windows, Linux, and macOS before 26.
- **macOS 26+** uses `src-tauri/icons/AppIcon.icon` (Icon Composer), which switches between light, dark, clear and tinted itself.
- **Windows:** the window icon follows each window's light or dark appearance.
- **About window:** follows the system appearance.

All the icon files are generated by `src-tauri/icons/make_icon.py`; its docstring lists the commands to regenerate them.

## Third-party notices

`THIRD-PARTY-NOTICES.txt` is compiled into the About window. Regenerate it after changing dependencies:

```sh
cargo install cargo-about --features cli   # once
tools/third-party-notices.sh
```

## License

MIT, see [LICENSE](LICENSE). Copyright (c) 2026 it-problems.

Bundled third-party code: [xterm.js](https://github.com/xtermjs/xterm.js) and `@xterm/addon-fit` (MIT), in `ui/vendor/xterm` with their license.
