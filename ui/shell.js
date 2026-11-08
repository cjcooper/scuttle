// Shell window (shell.html?label=shell-N): an xterm.js terminal wired to a
// PTY running `docker exec -it` / `docker run -it` in the backend.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const currentWindow = window.__TAURI__.window.getCurrentWindow();

const $ = (s) => document.querySelector(s);
const label = new URLSearchParams(location.search).get("label");

let generation = 0;
/** Events received while (re)connecting, or null when connected. */
let pending = [];
let session = null;
let options = { user: "", shell: "auto" };

// ---------------------------------------------------------------- terminal

const dark = matchMedia("(prefers-color-scheme: dark)");
function theme() {
  if (dark.matches) return { background: "#1d2128", foreground: "#e3e7ed", cursor: "#5b9bff", selectionBackground: "#2f4a7a" };
  // xterm's default ANSI colours are tuned for dark backgrounds; darken them on white.
  return {
    background: "#ffffff", foreground: "#1c2430", cursor: "#2f6fe4", selectionBackground: "#cfe0fb",
    black: "#1c2430", red: "#c6362c", green: "#1d7a3f", yellow: "#8a5300", blue: "#2457c5", magenta: "#9b3fb5",
    cyan: "#0f7285", white: "#6b7686", brightBlack: "#4b5563", brightRed: "#a8261d", brightGreen: "#166534",
    brightYellow: "#734500", brightBlue: "#1d4ab0", brightMagenta: "#7e2f96", brightCyan: "#0b5d6c", brightWhite: "#1c2430",
  };
}

const term = new Terminal({
  cursorBlink: true,
  fontFamily: '"SF Mono", Menlo, Consolas, monospace',
  fontSize: 13,
  scrollback: 10000,
  macOptionIsMeta: true,
  theme: theme(),
});
const fit = new FitAddon.FitAddon();
term.loadAddon(fit);
term.open($("#term"));
fit.fit();
dark.addEventListener("change", () => (term.options.theme = theme()));
new ResizeObserver(() => fit.fit()).observe($("#term"));

term.onData((data) => invoke("shell_input", { data }));
term.onResize(({ cols, rows }) => invoke("shell_resize", { cols, rows }));

function decode(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

// ------------------------------------------------------------------ header

function renderOptions() {
  const known = ["", "root", "host"];
  const mode = known.includes(options.user) ? options.user : "custom";
  document.querySelectorAll("#user-seg button").forEach((b) => b.classList.toggle("active", b.dataset.user === mode));
  $("#custom-user").hidden = mode !== "custom";
  if (mode === "custom") $("#custom-user").value = options.user;
  const sel = $("#shell-select");
  if (![...sel.options].some((o) => o.value === options.shell)) {
    sel.append(new Option(options.shell, options.shell));
  }
  sel.value = options.shell;
  // The shell choice only applies to exec and "shell into image" sessions.
  const t = session?.target;
  sel.hidden = !!t && t.kind === "run" && !t.shellEntrypoint;
}

function userLabel(u) {
  return { "": "default user", root: "root", host: "your user" }[u] ?? u;
}

const SIGN_IN = /AWS sign-in required for profile `([^`]+)`/;
let signInProfile = null;

function ended(message) {
  const box = $("#shell-ended");
  box.hidden = !message;
  $("#shell-ended-text").textContent = message ?? "";
  // An expired AWS SSO session: offer sign-in, then reconnect.
  signInProfile = message?.match(SIGN_IN)?.[1] ?? null;
  $("#sso").hidden = !signInProfile;
}

$("#sso").onclick = async () => {
  const profile = signInProfile;
  $("#sso").disabled = true;
  term.write(`\r\n\x1b[2m$ aws sso login --profile ${profile} (complete it in your browser)\x1b[0m\r\n`);
  try {
    await invoke("aws_sso_login", { profile, taskId: `sso-${label}` });
    await connect(options);
  } catch (e) {
    ended(String(e));
  } finally {
    $("#sso").disabled = false;
  }
};

async function connect(newOptions) {
  ended(null);
  term.reset();
  // Ignore the old connection's output and exit while the new one starts:
  // the backend kills it, and that exit must not close the window.
  generation = -1;
  pending = [];
  if (newOptions) {
    term.write(`\x1b[2mReconnecting as ${userLabel(newOptions.user)}…\x1b[0m\r\n`);
  }
  try {
    session = await invoke("shell_start", { cols: term.cols, rows: term.rows, options: newOptions ?? null });
  } catch (e) {
    pending = null;
    ended(String(e));
    return;
  }
  generation = session.generation;
  // Replay what the new connection sent before we knew its generation.
  const early = pending.filter((out) => out.generation === generation);
  pending = null;
  early.forEach(handleOutput);
  options = session.options;
  $("#shell-heading").textContent = `${session.title} — ${userLabel(options.user)}`;
  $("#shell-command").textContent = session.command;
  $("#shell-command").title = session.command;
  document.title = `${session.title} (${userLabel(options.user)})`;
  renderOptions();
  term.focus();
}

document.querySelectorAll("#user-seg button").forEach((b) => (b.onclick = () => {
  if (b.dataset.user === "custom") {
    $("#custom-user").hidden = false;
    $("#custom-user").focus();
    return;
  }
  connect({ ...options, user: b.dataset.user });
}));
$("#custom-user").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && e.target.value.trim()) connect({ ...options, user: e.target.value.trim() });
});
$("#shell-select").onchange = (e) => connect({ ...options, shell: e.target.value });
$("#reconnect").onclick = () => connect(options);
$("#pop-out").onclick = () => invoke("shell_pop_out").catch((e) => ended(String(e)));

/** Output for the current connection. */
function handleOutput(out) {
  if (out.type === "data") {
    term.write(decode(out.data));
  } else if (out.type === "exited") {
    if (out.clean) {
      currentWindow.close();
    } else {
      term.write(`\r\n\x1b[2m[${out.message}]\x1b[0m\r\n`);
      ended(`${out.message} Reconnect to start again.`);
    }
  }
}

// -------------------------------------------------------------------- boot

(async () => {
  await listen(label, (e) => {
    if (pending) pending.push(e.payload); // reconnecting: sort out once we know the generation
    else if (e.payload.generation === generation) handleOutput(e.payload);
  });
  await connect();
})();
