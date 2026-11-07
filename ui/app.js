// Dashboard UI. Talks to the Rust backend via Tauri's global API
// (app.withGlobalTauri = true), so no bundler is required.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const dialog = window.__TAURI__.dialog;

const $ = (sel) => document.querySelector(sel);
const el = (tag, props = {}, ...children) => {
  const n = Object.assign(document.createElement(tag), props);
  for (const c of children) n.append(c instanceof Node ? c : document.createTextNode(c ?? ""));
  return n;
};

let status = null;
let providers = [];
let images = [];
const selectedImages = new Set();
let taskSeq = 0;
const newTask = (kind) => `${kind}-${Date.now()}-${++taskSeq}`;

// ------------------------------------------------------------------ helpers

function toast(msg, isError = false) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = "toast show" + (isError ? " error" : "");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.className = "toast"), isError ? 6000 : 3000);
}

function bytes(n) {
  if (n == null) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return `${n % 1 === 0 ? n : n.toFixed(1)} ${units[i]}`;
}

async function call(cmd, args, { quiet = false } = {}) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    if (!quiet) toast(String(e), true);
    throw e;
  }
}

async function confirmAction(message, title = "Confirm") {
  if (dialog?.ask) return dialog.ask(message, { title, kind: "warning" });
  return window.confirm(message);
}

const engineUp = () => !!status?.engineVersion;

function logActivity(line) {
  const log = $("#activity-log");
  const stick = log.scrollTop + log.clientHeight >= log.scrollHeight - 4;
  log.append(line + "\n");
  // Keep the activity buffer bounded.
  if (log.textContent.length > 200_000) log.textContent = log.textContent.slice(-150_000);
  if (stick) log.scrollTop = log.scrollHeight;
  const n = (logActivity.count = (logActivity.count || 0) + 1);
  $("#activity-count").textContent = `(${n})`;
}

// --------------------------------------------------------------- navigation

document.querySelectorAll(".nav").forEach((b) =>
  b.addEventListener("click", () => showView(b.dataset.view))
);

function showView(name) {
  document.querySelectorAll(".nav").forEach((b) => b.classList.toggle("active", b.dataset.view === name));
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("active", v.id === `view-${name}`));
  if (name === "images") loadImages();
  if (name === "containers") { loadContainers(); renderPresets(); }
  if (name === "runtime") loadProfiles();
}

// Sub-tabs inside a view (Images: Local/Build, Containers: Containers/Run).
document.querySelectorAll(".tabs").forEach((group) =>
  group.querySelectorAll(".tab").forEach((t) => (t.onclick = () => showTab(t.dataset.tab)))
);
function showTab(id) {
  const target = document.getElementById(id);
  const view = target.closest(".view");
  showView(view.id.replace("view-", ""));
  view.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.tab === id));
  view.querySelectorAll(".subview").forEach((v) => v.classList.toggle("active", v === target));
  if (id === "containers-run") updatePreview();
}

// ------------------------------------------------------------------ status

function renderStatus(s) {
  status = s;
  const dot = $("#status-dot");
  dot.className = "dot " + (s.busy ? "busy" : s.vmState);
  const state = s.busy ? `${s.busy}…` : s.vmState[0].toUpperCase() + s.vmState.slice(1);
  $("#status-text").textContent = `${s.providerName} · ${s.profile} · ${state}`;

  const grid = $("#status-grid");
  grid.replaceChildren(
    ...[
      ["Provider", s.providerName],
      ["Profile", s.profile],
      ["State", state],
      ["Engine", s.engineVersion ? `Docker API ${s.engineVersion}` : "Not reachable"],
      ...(s.engineError && s.vmState !== "stopped" ? [["Last error", s.engineError]] : []),
    ].map(([k, v]) => el("div", {}, el("span", {}, k), el("span", {}, v)))
  );

  const running = s.vmState === "running";
  $("#vm-start").disabled = !s.managesVm || !!s.busy || running;
  $("#vm-stop").disabled = !s.managesVm || !!s.busy || !running;
  $("#vm-restart").disabled = !s.managesVm || !!s.busy || !running;
  for (const id of ["#prune-dangling", "#prune-unused", "#prune-cache", "#build-submit", "#pull-submit", "#run-submit"]) {
    $(id).disabled = !engineUp() || !!s.busy;
  }
}

async function vmAction(action) {
  try {
    await call("vm_action", { action, taskId: newTask("vm") });
    toast(`${action[0].toUpperCase() + action.slice(1)} complete`);
  } finally {
    loadProfiles();
  }
}
$("#vm-start").onclick = () => vmAction("start");
$("#vm-stop").onclick = () => vmAction("stop");
$("#vm-restart").onclick = () => vmAction("restart");

// ---------------------------------------------------------- provider/profile

async function loadProviders() {
  providers = await call("list_providers");
  const sel = await call("get_selection");
  const ps = $("#provider-select");
  ps.replaceChildren(
    ...providers.map((p) =>
      el("option", { value: p.id, disabled: !p.installed, selected: p.id === sel.provider },
        p.installed ? p.name : `${p.name} (not installed)`)
    )
  );
  await loadProfiles(sel);
}

async function loadProfiles(sel) {
  sel ??= await call("get_selection", undefined, { quiet: true });
  const providerId = $("#provider-select").value || sel.provider;
  const provider = providers.find((p) => p.id === providerId);
  $("#provider-hint").textContent = provider?.managesVm
    ? ""
    : "This provider's engine lifecycle is managed by its own app (Docker Desktop, OrbStack, Rancher Desktop…). Images, builds and containers still work here.";
  let profiles = [];
  try {
    profiles = await call("list_profiles", { provider: providerId }, { quiet: true });
  } catch (e) {
    $("#profiles-body").replaceChildren(el("tr", {}, el("td", { colSpan: 7, className: "empty" }, String(e))));
  }
  const current = providerId === sel.provider ? sel.profile : profiles.find((p) => p.isDefault)?.name;
  $("#profile-select").replaceChildren(
    ...profiles.map((p) => el("option", { value: p.name, selected: p.name === current }, p.name))
  );
  if (profiles.length) {
    $("#profiles-body").replaceChildren(
      ...profiles.map((p) =>
        el("tr", {},
          el("td", {}, p.name + (p.isDefault ? " (default)" : "")),
          el("td", {}, el("span", { className: `badge ${p.state}` }, p.state)),
          el("td", {}, p.arch ?? "—"),
          el("td", {}, p.cpus ?? "—"),
          el("td", {}, bytes(p.memory)),
          el("td", {}, bytes(p.disk)),
          el("td", { className: "mono" }, [p.runtime, p.detail].filter(Boolean).join(" · ") || "—"))
      )
    );
  } else if (!$("#profiles-body").querySelector(".empty")) {
    $("#profiles-body").replaceChildren(el("tr", {}, el("td", { colSpan: 7, className: "empty" }, "No profiles found")));
  }
}

async function applySelection() {
  const provider = $("#provider-select").value;
  const profile = $("#profile-select").value;
  if (!provider || !profile) return;
  renderStatus(await call("set_selection", { selection: { provider, profile } }));
}

$("#provider-select").onchange = async () => {
  await loadProfiles({ provider: "", profile: "" });
  await applySelection();
};
$("#profile-select").onchange = applySelection;
$("#refresh-profiles").onclick = async () => {
  await loadProfiles();
  renderStatus(await call("get_status"));
};

// ------------------------------------------------------------------ images

async function loadImages() {
  if (!engineUp()) {
    $("#images-body").replaceChildren(el("tr", {}, el("td", { colSpan: 8, className: "empty" }, "Engine not running")));
    $("#disk-usage").replaceChildren();
    return;
  }
  const [imgs, usage] = await Promise.all([
    call("list_images", undefined, { quiet: true }).catch((e) => (toast(String(e), true), [])),
    call("disk_usage", undefined, { quiet: true }).catch(() => []),
  ]);
  images = imgs;
  $("#image-names").replaceChildren(
    ...images.filter((i) => i.repository !== "<none>").map((i) => el("option", { value: imageRef(i) }))
  );
  for (const id of [...selectedImages]) if (!images.some((i) => i.id === id)) selectedImages.delete(id);
  renderImages();
  $("#disk-usage").replaceChildren(
    ...usage.map((u) =>
      el("div", {}, el("small", {}, u.kind), el("b", {}, u.size), el("small", {}, `${u.totalCount} total · ${u.reclaimable} reclaimable`))
    )
  );
}

function renderImages() {
  const q = $("#images-filter").value.trim().toLowerCase();
  const rows = images.filter((i) => !q || `${i.repository}:${i.tag} ${i.id}`.toLowerCase().includes(q));
  $("#images-body").replaceChildren(
    ...(rows.length
      ? rows.map((i) => {
          const cb = el("input", { type: "checkbox", checked: selectedImages.has(i.id) });
          const tr = el("tr", { className: selectedImages.has(i.id) ? "selected" : "" },
            el("td", {}, cb),
            el("td", {}, i.repository),
            el("td", {}, i.tag),
            el("td", { className: "mono" }, i.id.replace(/^sha256:/, "").slice(0, 12)),
            el("td", {}, i.size),
            el("td", {}, i.createdSince),
            el("td", {}, i.containers && i.containers !== "0" && i.containers !== "N/A" ? i.containers : ""),
            el("td", { className: "actions" }, ...imageActions(i)));
          cb.onchange = () => {
            cb.checked ? selectedImages.add(i.id) : selectedImages.delete(i.id);
            tr.classList.toggle("selected", cb.checked);
            updateImageButtons();
          };
          return tr;
        })
      : [el("tr", {}, el("td", { colSpan: 8, className: "empty" }, images.length ? "No matches" : "No images"))])
  );
  updateImageButtons();
}

const imageRef = (i) => (i.repository === "<none>" ? i.id : i.tag === "<none>" ? i.repository : `${i.repository}:${i.tag}`);

function imageActions(i) {
  const ref = imageRef(i);
  const shell = el("button", { className: "small", title: `Throwaway container with a shell as ${userLabel()}` }, "Shell");
  shell.onclick = () => imageShell(ref);
  const run = el("button", { className: "small secondary" }, "Run…");
  run.onclick = () => {
    fillRunForm({ image: ref, interactive: true, remove: true });
    showTab("containers-run");
  };
  return [shell, run];
}

async function imageShell(ref) {
  await call("image_shell", { image: ref });
  toast(`Opening shell in ${ref} as ${userLabel()}`);
}

function updateImageButtons() {
  $("#images-remove").disabled = selectedImages.size === 0;
  $("#images-remove").textContent = selectedImages.size ? `Remove selected (${selectedImages.size})` : "Remove selected";
}

$("#images-filter").oninput = renderImages;
$("#images-refresh").onclick = loadImages;
$("#images-all").onchange = (e) => {
  const q = $("#images-filter").value.trim().toLowerCase();
  for (const i of images) {
    if (q && !`${i.repository}:${i.tag} ${i.id}`.toLowerCase().includes(q)) continue;
    e.target.checked ? selectedImages.add(i.id) : selectedImages.delete(i.id);
  }
  renderImages();
};

$("#images-remove").onclick = async () => {
  const ids = [...selectedImages];
  const force = $("#force-remove").checked;
  if (!(await confirmAction(`Remove ${ids.length} image(s)${force ? " (forced)" : ""}?`))) return;
  try {
    const out = await call("remove_images", { ids, force });
    out.split("\n").filter(Boolean).forEach(logActivity);
    selectedImages.clear();
    toast(`Removed ${ids.length} image(s)`);
  } finally {
    loadImages();
  }
};

async function prune(cmd, args, label) {
  try {
    await call(cmd, { ...args, taskId: newTask("prune") });
    toast(`${label} complete`);
  } finally {
    loadImages();
  }
}
$("#prune-dangling").onclick = () => prune("prune_images", { all: false }, "Remove dangling images");
$("#prune-unused").onclick = async () => {
  if (!(await confirmAction("Remove ALL images not used by any container? They will need to be pulled or rebuilt again.", "Remove unused images"))) return;
  prune("prune_images", { all: true }, "Remove unused images");
};
$("#prune-cache").onclick = () => prune("prune_build_cache", {}, "Prune build cache");

// ------------------------------------------------------------------- build

let buildTask = null;

$("#pick-context").onclick = async () => {
  const dir = await dialog.open({ directory: true, title: "Choose build context" });
  if (dir) $("#build-context").value = dir;
};
$("#pick-dockerfile").onclick = async () => {
  const file = await dialog.open({ title: "Choose Dockerfile", defaultPath: $("#build-context").value || undefined });
  if (file) {
    $("#build-dockerfile").value = file;
    if (!$("#build-context").value) $("#build-context").value = file.replace(/[\\/][^\\/]*$/, "");
  }
};

$("#build-form").onsubmit = async (e) => {
  e.preventDefault();
  const context = $("#build-context").value.trim();
  let dockerfile = $("#build-dockerfile").value.trim();
  // Docker resolves -f against the CWD, so anchor relative paths to the context.
  if (dockerfile && !/^([\\/]|[A-Za-z]:)/.test(dockerfile)) dockerfile = `${context}/${dockerfile}`;
  const request = {
    context,
    dockerfile: dockerfile || null,
    tags: $("#build-tags").value.split(",").map((s) => s.trim()).filter(Boolean),
    platform: $("#build-platform").value.trim() || null,
    target: $("#build-target").value.trim() || null,
    buildArgs: $("#build-args").value.split("\n").map((s) => s.trim()).filter(Boolean),
    noCache: $("#build-nocache").checked,
    pull: $("#build-pull").checked,
  };
  $("#build-log").textContent = "";
  buildTask = newTask("build");
  $("#build-submit").disabled = true;
  $("#build-submit").textContent = "Building…";
  try {
    await call("build_image", { request, taskId: buildTask });
    toast("Build succeeded");
  } finally {
    $("#build-submit").textContent = "Build";
    $("#build-submit").disabled = !engineUp();
  }
};

// -------------------------------------------------------------------- pull

$("#pull-form").onsubmit = async (e) => {
  e.preventDefault();
  const image = $("#pull-image").value.trim();
  if (!image) return;
  $("#pull-submit").disabled = true;
  $("#activity").open = true;
  try {
    await call("pull_image", { image, awsProfile: awsProfile("pull"), taskId: newTask("pull") });
    toast(`Pulled ${image}`);
    loadImages();
  } finally {
    $("#pull-submit").disabled = !engineUp();
  }
};

// ------------------------------------------------------ shell preferences

// Defaults for new shell windows (changed from a shell window or the tray).
let prefs = { terminal: "auto", shellUser: "", customUser: "", shell: "auto" };
let presets = [];

function userLabel() {
  return { "": "image default user", root: "root", host: "your user" }[prefs.shellUser] ?? (prefs.customUser || "custom user");
}

async function loadSettings() {
  const s = await call("get_settings");
  prefs = s.preferences;
  presets = s.presets;
  renderPresets();
}

// ------------------------------------------------------------- AWS / ECR

/**
 * Show an AWS profile picker under `prefix` (pull / run) while `image` is an
 * ECR image. Returns once the picker reflects the image.
 */
async function updateAws(prefix, image, preferred) {
  const row = $(`#${prefix}-aws`);
  const info = image ? await call("ecr_info", { image }, { quiet: true }).catch(() => null) : null;
  row.hidden = !info;
  if (!info) return;
  const sel = $(`#${prefix}-aws-profile`);
  const want = preferred ?? (row.dataset.registry === info.registry.host ? sel.value : null) ?? info.profile;
  row.dataset.registry = info.registry.host;
  const names = [...info.profiles];
  if (want && !names.includes(want)) names.unshift(want);
  sel.replaceChildren(...(names.length ? names : [""]).map((p) => el("option", { value: p }, p || "(no profiles in ~/.aws)")));
  sel.value = want ?? info.profile ?? names[0] ?? "";
  $(`#${prefix}-aws-hint`).textContent = `ECR account ${info.registry.account} · ${info.registry.region}`;
}

function awsProfile(prefix) {
  return $(`#${prefix}-aws`).hidden ? null : $(`#${prefix}-aws-profile`).value || null;
}

for (const prefix of ["pull", "run"]) {
  $(`#${prefix}-aws-sso`).onclick = async () => {
    const profile = awsProfile(prefix);
    if (!profile) return;
    $("#activity").open = true;
    await call("aws_sso_login", { profile, taskId: newTask("sso") });
    toast(`Signed in to AWS profile ${profile}`);
  };
}

let awsTimer = {};
function debounceAws(prefix, image) {
  clearTimeout(awsTimer[prefix]);
  awsTimer[prefix] = setTimeout(() => updateAws(prefix, image), 200);
}
$("#pull-image").addEventListener("input", (e) => debounceAws("pull", e.target.value.trim()));
$("#run-image").addEventListener("input", (e) => debounceAws("run", e.target.value.trim()));

// -------------------------------------------------------------- containers

async function loadContainers() {
  const body = $("#containers-body");
  const empty = (msg) => body.replaceChildren(el("tr", {}, el("td", { colSpan: 6, className: "empty" }, msg)));
  if (!engineUp()) return empty("Engine not running");
  let list = [];
  try {
    list = await call("list_containers", undefined, { quiet: true });
  } catch (e) {
    return empty(String(e));
  }
  const q = $("#containers-filter").value.trim().toLowerCase();
  const runningOnly = $("#containers-running").checked;
  list = list.filter((c) => (!runningOnly || c.state === "running") && (!q || `${c.names} ${c.image}`.toLowerCase().includes(q)));
  if (!list.length) return empty(runningOnly ? "No running containers" : "No containers");
  body.replaceChildren(
    ...list.map((c) => {
      const running = c.state === "running";
      const btn = (label, action, cls = "secondary") => {
        const b = el("button", { className: `small ${cls}` }, label);
        b.onclick = async () => {
          if (action === "remove" && !(await confirmAction(`Remove container ${c.names}?`))) return;
          b.disabled = true;
          try {
            await call("container_action", { id: c.id, action });
          } finally {
            loadContainers();
          }
        };
        return b;
      };
      const shell = el("button", { className: "small", title: `docker exec as ${userLabel()}` }, "Shell");
      shell.onclick = async () => {
        await call("open_shell", { container: c.names || c.id });
        toast(`Opening shell in ${c.names} as ${userLabel()}`);
      };
      return el("tr", {},
        el("td", {}, c.names),
        el("td", { className: "mono" }, c.image),
        el("td", {}, el("span", { className: `badge ${c.state}` }, c.state)),
        el("td", {}, c.status),
        el("td", { className: "mono" }, c.ports),
        el("td", { className: "actions" },
          ...(running ? [shell, btn("Stop", "stop"), btn("Restart", "restart")] : [btn("Start", "start")]),
          btn("Remove", "remove", "danger")));
    })
  );
}
$("#containers-refresh").onclick = loadContainers;
$("#containers-filter").oninput = loadContainers;
$("#containers-running").onchange = loadContainers;

// --------------------------------------------------------------------- run

/** Split a string into shell words (handles '…', "…" and backslash escapes). */
function shellWords(str) {
  const out = [];
  let cur = "", quote = null, has = false;
  for (let i = 0; i < str.length; i++) {
    const ch = str[i];
    if (quote) {
      if (ch === quote) quote = null;
      else if (ch === "\\" && quote === '"' && i + 1 < str.length) cur += str[++i];
      else cur += ch;
    } else if (ch === "'" || ch === '"') { quote = ch; has = true; }
    else if (ch === "\\" && i + 1 < str.length) {
      const next = str[++i];
      if (next !== "\n") { cur += next; has = true; }
    } else if (/\s/.test(ch)) { if (has || cur) out.push(cur); cur = ""; has = false; }
    else { cur += ch; has = true; }
  }
  if (has || cur) out.push(cur);
  return out;
}

const quoteWord = (w) => (/^[A-Za-z0-9_\-./:=@%+,]+$/.test(w) ? w : `'${w.replace(/'/g, `'\\''`)}'`);
const lines = (v) => v.split("\n").map((s) => s.trim()).filter(Boolean);

// docker run flags that take no value; anything else unknown is assumed to take one.
const BOOL_FLAGS = new Set(["-i", "-t", "-d", "-P", "--interactive", "--tty", "--detach", "--rm", "--privileged",
  "--init", "--read-only", "--publish-all", "--no-healthcheck", "--oom-kill-disable", "--sig-proxy", "-q", "--quiet"]);

/** Parse `docker [container] run …` into a run request. */
function parseDockerRun(text) {
  let w = shellWords(text.replace(/\\\n/g, " "));
  let awsProfile = null;
  while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(w[0] ?? "")) {
    const [k, ...v] = w.shift().split("=");
    if (k === "AWS_PROFILE") awsProfile = v.join("=");
  }
  if (w[0] === "docker" || w[0]?.endsWith("/docker")) w = w.slice(1);
  if (w[0] === "container") w = w.slice(1);
  if (w[0] === "run") w = w.slice(1);
  const req = { image: "", name: null, user: "", entrypoint: null, command: [], ports: [], env: [], volumes: [],
    workdir: null, platform: null, network: null, interactive: false, remove: false, extraArgs: [], awsProfile: null };
  let detached = false, tty = false;
  for (let i = 0; i < w.length; i++) {
    let a = w[i];
    if (!a.startsWith("-") || a === "-") { req.image = a; req.command = w.slice(i + 1); break; }
    let val = null;
    const eq = a.startsWith("--") ? a.indexOf("=") : -1;
    if (eq > 0) { val = a.slice(eq + 1); a = a.slice(0, eq); }
    // Combined short flags such as -it / -itd
    if (/^-[a-zA-Z]{2,}$/.test(a) && [...a.slice(1)].every((c) => "itdP".includes(c))) {
      for (const c of a.slice(1)) { if (c === "i" || c === "t") tty = true; if (c === "d") detached = true; }
      continue;
    }
    const take = () => val ?? w[++i];
    switch (a) {
      case "-i": case "--interactive": case "-t": case "--tty": tty = true; break;
      case "-d": case "--detach": detached = true; break;
      case "--rm": req.remove = true; break;
      case "-u": case "--user": req.user = take(); break;
      case "--entrypoint": req.entrypoint = take(); break;
      case "--name": req.name = take(); break;
      case "-p": case "--publish": req.ports.push(take()); break;
      case "-e": case "--env": req.env.push(take()); break;
      case "-v": case "--volume": req.volumes.push(take()); break;
      case "-w": case "--workdir": req.workdir = take(); break;
      case "--platform": req.platform = take(); break;
      case "--network": case "--net": req.network = take(); break;
      default:
        if (val !== null) req.extraArgs.push(`${a}=${val}`);
        else if (BOOL_FLAGS.has(a)) req.extraArgs.push(a);
        else req.extraArgs.push(a, w[++i]);
    }
  }
  req.interactive = tty || !detached;
  req.awsProfile = awsProfile;
  return req;
}

function fillRunForm(r) {
  $("#run-image").value = r.image ?? "";
  $("#run-name").value = r.name ?? "";
  const user = r.user ?? "";
  const mode = ["", "root", "host"].includes(user) ? user : "custom";
  $("#run-user-mode").value = mode;
  $("#run-user-custom").value = mode === "custom" ? user : "";
  $("#run-user-custom-wrap").hidden = mode !== "custom";
  $("#run-entrypoint").value = r.entrypoint ?? "";
  $("#run-command").value = (r.command ?? []).map(quoteWord).join(" ");
  $("#run-ports").value = (r.ports ?? []).join("\n");
  $("#run-env").value = (r.env ?? []).join("\n");
  $("#run-volumes").value = (r.volumes ?? []).join("\n");
  $("#run-workdir").value = r.workdir ?? "";
  $("#run-platform").value = r.platform ?? "";
  $("#run-network").value = r.network ?? "";
  $("#run-extra").value = (r.extraArgs ?? []).map(quoteWord).join(" ");
  document.querySelector(`input[name="run-mode"][value="${r.interactive === false ? "detached" : "interactive"}"]`).checked = true;
  $("#run-rm").checked = !!r.remove;
  updateAws("run", $("#run-image").value.trim(), r.awsProfile ?? undefined);
  updatePreview();
}

function readRunForm() {
  const mode = $("#run-user-mode").value;
  const opt = (id) => $(id).value.trim() || null;
  return {
    image: $("#run-image").value.trim(),
    name: opt("#run-name"),
    user: mode === "custom" ? $("#run-user-custom").value.trim() : mode,
    entrypoint: opt("#run-entrypoint"),
    command: shellWords($("#run-command").value),
    ports: lines($("#run-ports").value),
    env: lines($("#run-env").value),
    volumes: lines($("#run-volumes").value),
    workdir: opt("#run-workdir"),
    platform: opt("#run-platform"),
    network: opt("#run-network"),
    interactive: document.querySelector('input[name="run-mode"]:checked').value === "interactive",
    remove: $("#run-rm").checked,
    extraArgs: shellWords($("#run-extra").value),
    awsProfile: awsProfile("run"),
  };
}

async function updatePreview() {
  const req = readRunForm();
  if (!req.image) { $("#run-preview").textContent = "docker container run … (choose an image)"; return; }
  try {
    $("#run-preview").textContent = await call("preview_run", { request: req }, { quiet: true });
  } catch (e) {
    $("#run-preview").textContent = String(e);
  }
}

$("#run-form").addEventListener("input", updatePreview);
$("#run-form").addEventListener("change", updatePreview);
$("#run-user-mode").addEventListener("change", (e) => ($("#run-user-custom-wrap").hidden = e.target.value !== "custom"));

$("#paste-parse").onclick = () => {
  const text = $("#paste-command").value.trim();
  if (!text) return;
  const req = parseDockerRun(text);
  if (!req.image) return toast("Couldn't find an image in that command", true);
  fillRunForm(req);
  toast("Form filled from command");
};
$("#paste-command").addEventListener("keydown", (e) => { if (e.key === "Enter") $("#paste-parse").click(); });

$("#run-copy").onclick = async () => {
  await updatePreview();
  await navigator.clipboard.writeText($("#run-preview").textContent);
  toast("Command copied");
};

async function runRequest(req) {
  const id = await call("run_container", { request: req });
  if (id) {
    toast(`Started ${id.slice(0, 12)}`);
    logActivity(`Started container ${id}`);
    loadContainers();
  } else {
    toast(`Opening ${req.image} in a shell window`);
  }
}

$("#run-form").onsubmit = (e) => {
  e.preventDefault();
  runRequest(readRunForm());
};

// ----------------------------------------------------------------- presets

$("#preset-save").onclick = async () => {
  const name = $("#preset-name").value.trim();
  if (!name) { $("#preset-name").focus(); return toast("Give the preset a name", true); }
  const request = readRunForm();
  if (!request.image) return toast("Choose an image first", true);
  presets = await call("save_preset", { preset: { name, request } });
  renderPresets();
  toast(`Saved preset “${name}” (also in the menu bar ▸ Run Preset)`);
};

async function renderPresets() {
  const body = $("#presets-body");
  if (!presets.length) {
    body.replaceChildren(el("tr", {}, el("td", { colSpan: 3, className: "empty" }, "No presets yet — fill the form and Save preset")));
    return;
  }
  const rows = await Promise.all(presets.map(async (p) => {
    const cmd = await call("preview_run", { request: p.request }, { quiet: true }).catch(String);
    const run = el("button", { className: "small" }, "Run");
    run.onclick = () => runRequest(p.request);
    const edit = el("button", { className: "small secondary" }, "Edit");
    edit.onclick = () => { fillRunForm(p.request); $("#preset-name").value = p.name; };
    const del = el("button", { className: "small danger" }, "Delete");
    del.onclick = async () => {
      if (!(await confirmAction(`Delete preset “${p.name}”?`))) return;
      presets = await call("delete_preset", { name: p.name });
      renderPresets();
    };
    return el("tr", {}, el("td", {}, p.name), el("td", { className: "cmd" }, cmd), el("td", { className: "actions" }, run, edit, del));
  }));
  body.replaceChildren(...rows);
}

// -------------------------------------------------------------------- boot

listen("status", (e) => {
  const wasUp = engineUp();
  renderStatus(e.payload);
  if (!wasUp && engineUp()) {
    const active = document.querySelector(".view.active")?.id;
    if (active === "view-images") loadImages();
    if (active === "view-containers") loadContainers();
  }
});

listen("task-log", (e) => {
  const { taskId, line } = e.payload;
  if (taskId === buildTask) {
    const log = $("#build-log");
    const stick = log.scrollTop + log.clientHeight >= log.scrollHeight - 4;
    log.append(line + "\n");
    if (stick) log.scrollTop = log.scrollHeight;
  } else {
    logActivity(line);
  }
});

listen("preferences", (e) => { prefs = e.payload; });

(async () => {
  await loadSettings();
  await loadProviders();
  renderStatus(await call("get_status"));
})();
