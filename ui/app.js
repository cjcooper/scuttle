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
  // An open modal sits in the top layer; show the toast inside it.
  const host = document.querySelector("dialog[open]") ?? document.body;
  if (t.parentElement !== host) host.append(t);
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

async function confirmAction(message, title = "Confirm", okLabel) {
  if (dialog?.ask) {
    return dialog.ask(message, { title, kind: "warning", ...(okLabel ? { okLabel, cancelLabel: "Cancel" } : {}) });
  }
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
  if (name === "history") loadHistory();
  if (name === "compose") loadStacks();
}

// Sub-tabs inside a view (Images: Local/Build, Containers: Containers/Run).
document.querySelectorAll(".tabs").forEach((group) =>
  group.querySelectorAll(".tab").forEach((t) => (t.onclick = () => showTab(t.dataset.tab)))
);
function showTab(id) {
  const target = document.getElementById(id);
  // Tabs live in a view or in a modal dialog.
  const view = target.closest(".view, dialog");
  if (view.classList.contains("view")) showView(view.id.replace("view-", ""));
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
  $("#vm-delete-data").hidden = s.providerId !== "colima";
  $("#vm-delete-data").disabled = !!s.busy;
  for (const id of ["#prune-dangling", "#prune-unused", "#prune-cache", "#build-submit", "#pull-submit", "#run-submit", "#run-pull"]) {
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

// colima delete --data for the selected profile.
$("#vm-delete-data").onclick = async () => {
  const profile = status?.profile;
  if (!profile || status?.providerId !== "colima") return;
  const ok = await confirmAction(
    `Delete ALL data for the Colima profile “${profile}”?\n\n` +
      "This runs “colima delete --data”: the VM is stopped and removed, along with every image, " +
      "container and volume in it. Images will need to be pulled or rebuilt again.\n\n" +
      "This can't be undone.",
    "Delete Colima data",
    "Delete Data"
  );
  if (!ok) return;
  $("#activity").open = true;
  try {
    await call("colima_delete", { profile, deleteData: true, taskId: newTask("colima") });
    toast(`Deleted ${profile} and all its data`);
  } finally {
    loadProfiles();
  }
};

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
  const colima = providerId === "colima";
  $("#configure-colima").hidden = !colima;
  $("#provider-hint").textContent = provider?.managesVm
    ? ""
    : "This provider's engine lifecycle is managed by its own app (Docker Desktop, OrbStack, Rancher Desktop…). Images, builds and containers still work here.";
  let profiles = [];
  try {
    profiles = await call("list_profiles", { provider: providerId }, { quiet: true });
  } catch (e) {
    $("#profiles-body").replaceChildren(el("tr", {}, el("td", { colSpan: 8, className: "empty" }, String(e))));
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
          el("td", { className: "mono" }, [p.runtime, p.detail].filter(Boolean).join(" · ") || "—"),
          el("td", { className: "actions" }, ...(colima ? [configureButton(p.name)] : [])))
      )
    );
  } else if (!$("#profiles-body").querySelector(".empty")) {
    $("#profiles-body").replaceChildren(el("tr", {}, el("td", { colSpan: 8, className: "empty" }, "No profiles found")));
  }
}

function configureButton(profile) {
  const b = el("button", { className: "small secondary" }, "Configure");
  b.onclick = () => window.openColimaDialog(profile);
  return b;
}

$("#configure-colima").onclick = () => window.openColimaDialog($("#profile-select").value || null);

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
  const named = i.repository !== "<none>" && i.tag !== "<none>";
  const pull = el("button", { className: "small secondary", disabled: !named, title: named ? `Pull ${ref} again for the latest` : "Untagged image" }, "Pull");
  pull.onclick = async () => { pull.disabled = true; await pullImage(ref); pull.disabled = false; };
  const del = el("button", { className: "small danger", title: `docker image rm ${ref}` }, "Delete");
  del.onclick = async () => { del.disabled = true; await deleteImage(i); del.disabled = false; };
  return [shell, run, pull, del];
}

/**
 * Delete one image row. Tagged rows are removed by `repo:tag`, so an image
 * with other tags keeps them; untagged rows by ID. If a stopped container
 * still uses it, offer to force the removal.
 */
async function deleteImage(i) {
  const ref = imageRef(i);
  const tagged = i.repository !== "<none>" && i.tag !== "<none>";
  const target = tagged ? ref : i.id;
  const label = tagged ? ref : `untagged image ${i.id.replace(/^sha256:/, "").slice(0, 12)}`;
  if (!(await confirmAction(`Delete ${label} (${i.size})?`, "Delete image", "Delete"))) return;
  const remove = async (force) => {
    const out = await invoke("remove_images", { ids: [target], force });
    out.split("\n").filter(Boolean).forEach(logActivity);
    selectedImages.delete(i.id);
    toast(`Deleted ${label}`);
  };
  try {
    await remove(false);
  } catch (e) {
    const msg = String(e);
    if (/cannot be forced|running container/i.test(msg)) {
      // A running container blocks it even with --force.
      toast(`${label} is used by a running container; stop or remove the container first.`, true);
    } else if (!/conflict|being used|is using its referenced image|must force/i.test(msg)) {
      toast(msg, true);
    } else if (await confirmAction(
      `${label} is used by a stopped container:\n\n${msg}\n\nForce the delete? The tag is removed; the container keeps working, and the image's space is freed once the container is removed.`,
      "Image in use", "Force Delete")) {
      try {
        await remove(true);
      } catch (e2) {
        toast(String(e2), true);
      }
    }
  } finally {
    loadImages();
  }
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

/**
 * Pull `image` without running it. `profile` is the AWS profile for
 * ecr-login registries (null = the one last used for the registry).
 */
async function pullImage(image, profile = null) {
  if (!image) return false;
  $("#activity").open = true;
  toast(`Pulling ${image}…`);
  try {
    await withAwsSignIn(() => invoke("pull_image", { image, awsProfile: profile, taskId: newTask("pull") }));
    toast(`Pulled ${image}`);
    loadImages();
    return true;
  } catch (e) {
    toast(String(e), true);
    return false;
  }
}

$("#pull-form").onsubmit = async (e) => {
  e.preventDefault();
  $("#pull-submit").disabled = true;
  try {
    await pullImage($("#pull-image").value.trim(), awsProfile("pull"));
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
  const r = info.registry;
  $(`#${prefix}-aws-hint`).textContent = r.account
    ? `docker-credential-ecr-login · account ${r.account} · ${r.region}`
    : `docker-credential-ecr-login · ${r.host}`;
}

const SIGN_IN = /AWS sign-in required for profile `([^`]+)`/;

/**
 * Run `action`; if it fails because an AWS profile's SSO session expired,
 * offer to sign in and try once more.
 */
async function withAwsSignIn(action) {
  try {
    return await action();
  } catch (e) {
    const m = String(e).match(SIGN_IN);
    if (!m) throw e;
    if (!(await confirmAction(`${e}\n\nSign in with “aws sso login --profile ${m[1]}” now? Your browser will open.`, "AWS sign-in"))) throw e;
    $("#activity").open = true;
    await call("aws_sso_login", { profile: m[1], taskId: newTask("sso") });
    return await action();
  }
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
  let id;
  try {
    id = await withAwsSignIn(() => invoke("run_container", { request: req }));
  } catch (e) {
    toast(String(e), true);
    throw e;
  }
  if (id) {
    toast(`Started ${id.slice(0, 12)}`);
    logActivity(`Started container ${id}`);
    loadContainers();
  } else {
    toast(`Opening ${req.image} in a shell window`);
  }
}

$("#run-pull").onclick = async () => {
  const image = $("#run-image").value.trim();
  if (!image) { $("#run-image").focus(); return toast("Choose an image first", true); }
  $("#run-pull").disabled = true;
  try {
    await pullImage(image, awsProfile("run"));
  } finally {
    $("#run-pull").disabled = !engineUp();
  }
};

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

// ----------------------------------------------------------------- history

let historyEntries = [];

function ago(ms) {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  if (s < 7 * 86400) return `${Math.floor(s / 86400)} d ago`;
  return new Date(ms).toLocaleDateString();
}

const KIND_LABEL = { run: "Run", pull: "Pull", shell: "Image shell", exec: "Container shell", compose: "Compose up" };

function historyActions(h) {
  const btn = (label, cls, fn, title) => {
    const b = el("button", { className: `small ${cls}`, title: title ?? "" }, label);
    b.onclick = async () => { b.disabled = true; try { await fn(); } finally { b.disabled = false; } };
    return b;
  };
  const out = [];
  if (h.kind === "run" && h.request) {
    out.push(btn("Run again", "", () => runRequest(h.request)));
    out.push(btn("Edit", "secondary", async () => { fillRunForm(h.request); showTab("containers-run"); }, "Open in the Run form"));
  }
  if (h.kind === "shell" && h.image) out.push(btn("Open shell", "", () => call("image_shell", { image: h.image }), `As ${userLabel()}`));
  if (h.kind === "exec" && h.container) out.push(btn("Open shell", "", () => call("open_shell", { container: h.container }), `As ${userLabel()}`));
  if (h.kind === "compose" && h.container) {
    out.push(btn("Up again", "", () => call("compose_action", { id: h.container, action: "up", taskId: newTask("compose") })));
  }
  if (h.image) out.push(btn(h.kind === "pull" ? "Pull again" : "Pull", h.kind === "pull" ? "" : "secondary", () => pullImage(h.image, h.awsProfile ?? null)));
  out.push(btn("Copy", "ghost", async () => { await navigator.clipboard.writeText(h.command); toast("Command copied"); }));
  out.push(btn("×", "ghost", async () => { historyEntries = await call("history_delete", { id: h.id }); renderHistory(); }, "Remove from history"));
  return out;
}

function renderHistory() {
  const q = $("#history-filter").value.trim().toLowerCase();
  const kind = $("#history-kind").value;
  const rows = historyEntries.filter((h) => (!kind || h.kind === kind) && (!q || h.command.toLowerCase().includes(q)));
  $("#history-body").replaceChildren(
    ...(rows.length
      ? rows.map((h) =>
          el("tr", { className: h.ok === false ? "failed" : "" },
            el("td", { className: "when", title: new Date(h.at).toLocaleString() }, ago(h.at)),
            el("td", {}, el("span", { className: `badge ${h.kind}` }, KIND_LABEL[h.kind] ?? h.kind)),
            el("td", { className: "cmd", title: h.error ?? "" }, h.command + (h.ok === false ? "  ✗" : "")),
            el("td", { className: "actions" }, ...historyActions(h))))
      : [el("tr", {}, el("td", { colSpan: 4, className: "empty" },
          historyEntries.length ? "No matches" : "Nothing yet: runs, pulls and shells you start appear here."))])
  );
}

async function loadHistory() {
  historyEntries = await call("history_list", undefined, { quiet: true }).catch(() => []);
  renderHistory();
}

$("#history-filter").oninput = renderHistory;
$("#history-kind").onchange = renderHistory;
$("#history-clear").onclick = async () => {
  if (!(await confirmAction("Clear the whole command history?", "Clear history"))) return;
  historyEntries = await call("history_clear");
  renderHistory();
};
listen("history", (e) => { historyEntries = e.payload; if ($("#view-history").classList.contains("active")) renderHistory(); });

// ------------------------------------------------------------ open at login

let loginItem = null;

function renderLoginItem(item) {
  loginItem = item;
  $("#login-enabled").checked = item.enabled;
  $("#login-start").checked = item.startProvider;
  $("#login-start").disabled = !item.enabled;
  const manages = status?.managesVm;
  $("#login-start-label").textContent = manages
    ? `…and start ${status.providerName} (${status.profile})`
    : "…and start the selected profile (Colima / Podman only)";
}

async function saveLoginItem() {
  try {
    renderLoginItem(await call("login_item_set", {
      enabled: $("#login-enabled").checked,
      startProvider: $("#login-start").checked,
    }));
  } catch {
    renderLoginItem(await call("login_item_get"));
  }
}
$("#login-enabled").onchange = saveLoginItem;
$("#login-start").onchange = saveLoginItem;
listen("login-item", (e) => renderLoginItem(e.payload));
// The label names the selected provider/profile.
listen("status", () => {
  if (loginItem) renderLoginItem(loginItem);
  else call("login_item_get", undefined, { quiet: true }).then(renderLoginItem).catch(() => {});
});

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
// The menu bar can ask the dashboard to show a page (e.g. Compose).
listen("navigate", (e) => showView(e.payload));

(async () => {
  await loadSettings();
  await loadProviders();
  renderStatus(await call("get_status"));
})();
