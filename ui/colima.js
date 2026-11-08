// Colima view: edit a profile's colima.yaml (resources, VM, Kubernetes,
// network, mounts, env, Docker daemon config, provision scripts), or create a
// new profile. Shares helpers ($, el, call, toast, confirmAction, newTask,
// loadProfiles, showTab) with app.js.

const NEW = "__new__";
const GIB = 1024 ** 3;

let cfgProfiles = [];   // from list_profiles("colima")
let cfgProfile = null;  // selected profile name or NEW
let cfgFile = null;     // { path, config, raw } as loaded
let cfgModel = null;    // working copy of config
let cfgHost = { cpus: 8, memory: null };
let cfgLoaded = false;

const isNew = () => cfgProfile === NEW;

// ------------------------------------------------------------------ schema

/** Field types: int, float, text, select, bool, lines, kv, json. */
const SECTIONS = [
  { title: "Resources", fields: [
    { key: "cpu", label: "CPUs", type: "int", min: 1, max: () => cfgHost.cpus, range: true, unit: "" },
    { key: "memory", label: "Memory", type: "float", min: 0.5, step: 0.5, max: () => Math.max(2, Math.floor((cfgHost.memory ?? 16 * GIB) / GIB)), range: true, unit: "GiB" },
    { key: "disk", label: "Disk", type: "int", min: 1, max: () => 1024, range: true, unit: "GiB",
      help: () => (isNew() ? "Container data disk." : `Can only grow (currently ${cfgFile?.config.disk} GiB).`),
      minFn: () => (isNew() ? 1 : cfgFile?.config.disk ?? 1) },
    { key: "rootDisk", label: "Root disk", type: "int", min: 1, unit: "GiB", help: "VM root filesystem." },
  ]},
  { title: "Virtual machine", fields: [
    { key: "arch", label: "Architecture", type: "select", options: ["host", "aarch64", "x86_64"], createOnly: true },
    { key: "vmType", label: "VM type", type: "select", options: ["vz", "qemu", "krunkit"], createOnly: true,
      help: "vz: macOS Virtualization.framework · krunkit: GPU access" },
    { key: "runtime", label: "Runtime", type: "select", options: ["docker", "containerd", "incus"], createOnly: true,
      help: "This app manages docker-runtime profiles." },
    { key: "mountType", label: "Mount type", type: "select", options: ["virtiofs", "9p", "sshfs"], createOnly: true },
    { key: "cpuType", label: "CPU type", type: "text", placeholder: "host", help: "qemu only." },
    { key: "hostname", label: "Hostname", type: "text", placeholder: "colima" },
    { key: "modelRunner", label: "AI model runner", type: "select", options: ["docker", "ramalama"] },
    { key: "diskImage", label: "Custom disk image", type: "text", placeholder: "(download default)" },
    { key: "rosetta", label: "Rosetta", type: "bool", help: "amd64 emulation via Rosetta (Apple silicon, vz)." },
    { key: "binfmt", label: "binfmt emulation", type: "bool", help: "Foreign-architecture emulation." },
    { key: "nestedVirtualization", label: "Nested virtualization", type: "bool", help: "M3+ and vz." },
    { key: "mountInotify", label: "Propagate inotify events", type: "bool", help: "Experimental." },
  ]},
  { title: "Kubernetes", fields: [
    { key: "kubernetes.enabled", label: "Enable Kubernetes (k3s)", type: "bool" },
    { key: "kubernetes.version", label: "Version", type: "text", placeholder: "latest stable, e.g. v1.35.0+k3s1" },
    { key: "kubernetes.port", label: "API port", type: "int", min: 0, help: "0 = random free port." },
    { key: "kubernetes.k3sArgs", label: "k3s arguments", type: "lines", help: "One per line.", wide: true },
  ]},
  { title: "Network", fields: [
    { key: "network.mode", label: "Mode", type: "select", options: ["shared", "bridged"] },
    { key: "network.interface", label: "Bridged interface", type: "text", placeholder: "en0" },
    { key: "network.gatewayAddress", label: "Gateway address", type: "text", placeholder: "192.168.5.2", help: "Last octet must be 2." },
    { key: "network.address", label: "Reachable IP address", type: "bool", help: "Assign an IP reachable from the host." },
    { key: "network.preferredRoute", label: "Prefer assigned IP route", type: "bool" },
    { key: "network.hostAddresses", label: "Replicate host addresses", type: "bool", help: "Port-forward to specific host IPs." },
    { key: "network.dns", label: "DNS resolvers", type: "lines", help: "One per line, e.g. 1.1.1.1" },
    { key: "network.dnsHosts", label: "DNS hosts", type: "kv", help: "name=target per line." },
  ]},
  { title: "Mounts", custom: "mounts" },
  { title: "Environment", fields: [
    { key: "env", label: "VM environment variables", type: "kv", help: "KEY=value per line.", wide: true },
  ]},
  { title: "Docker daemon (daemon.json)", fields: [
    { key: "docker", label: "Settings", type: "json", wide: true,
      help: 'e.g. {"insecure-registries": ["myregistry:5000"], "features": {"buildkit": true}}' },
  ]},
  { title: "Provision scripts", custom: "provision" },
  { title: "Other", fields: [
    { key: "autoActivate", label: "Set as active docker context on start", type: "bool" },
    { key: "forwardAgent", label: "Forward SSH agent", type: "bool" },
    { key: "sshConfig", label: "Add to ~/.ssh/config", type: "bool" },
    { key: "sshPort", label: "SSH port", type: "int", min: 0, help: "0 = random." },
    { key: "portForwarder", label: "Port forwarder", type: "select", options: ["ssh", "grpc", "none"], help: "grpc adds UDP (experimental)." },
  ]},
];

// ------------------------------------------------------------------ model

const getPath = (obj, path) => path.split(".").reduce((o, k) => o?.[k], obj);
function setPath(obj, path, value) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((o, k) => (o[k] ??= {}), obj)[last] = value;
}

const kvToText = (m) => Object.entries(m ?? {}).map(([k, v]) => `${k}=${v}`).join("\n");
function textToKv(t) {
  const out = {};
  for (const line of t.split("\n").map((l) => l.trim()).filter(Boolean)) {
    const i = line.indexOf("=");
    if (i > 0) out[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  }
  return out;
}

const dirtySettings = () => cfgModel && cfgFile && JSON.stringify(cfgModel) !== JSON.stringify(cfgFile.config);
const dirtyYaml = () => cfgFile && $("#cfg-raw").value !== cfgFile.raw;

function updateDirty() {
  const d = $("#cfg-dirty");
  const yamlTab = $("#colima-yaml").classList.contains("active");
  let msg = "";
  if (isNew()) msg = "New profile: Create & Start creates the VM with these settings.";
  else if (yamlTab ? dirtyYaml() : dirtySettings()) msg = "Unsaved changes. Saved settings take effect when the profile restarts.";
  else if ((yamlTab ? dirtySettings() : dirtyYaml())) msg = `Unsaved changes in the ${yamlTab ? "Settings" : "YAML"} tab.`;
  d.textContent = msg;
  d.classList.toggle("cfg-dirty-on", !!msg && !isNew());
}

// ------------------------------------------------------------------ render

function fieldEl(f) {
  const value = getPath(cfgModel, f.key);
  const locked = f.createOnly && !isNew();
  const help = typeof f.help === "function" ? f.help() : f.help;
  const onChange = (v) => { setPath(cfgModel, f.key, v); updateDirty(); };
  const wrap = (input) =>
    el("label", { className: "cfg-field" + (f.wide ? " wide" : "") },
      el("span", {}, f.label + (locked ? " (fixed)" : "")), input, ...(help ? [el("small", {}, help)] : []));

  switch (f.type) {
    case "bool": {
      const cb = el("input", { type: "checkbox", checked: !!value, disabled: locked });
      cb.onchange = () => onChange(cb.checked);
      return el("label", { className: "cfg-field check" + (f.wide ? " wide" : "") }, cb,
        el("div", {}, el("span", {}, f.label), ...(help ? [el("small", {}, help)] : [])));
    }
    case "select": {
      const opts = [...f.options];
      if (value && !opts.includes(value)) opts.push(value);
      const sel = el("select", { disabled: locked }, ...opts.map((o) => el("option", { value: o, selected: o === value }, o)));
      sel.onchange = () => onChange(sel.value);
      return wrap(sel);
    }
    case "int":
    case "float": {
      const parse = (v) => (f.type === "int" ? parseInt(v, 10) : parseFloat(v));
      const min = f.minFn ? f.minFn() : f.min;
      const num = el("input", { type: "number", value, min, step: f.step ?? 1 });
      if (!f.range) {
        num.oninput = () => !isNaN(parse(num.value)) && onChange(parse(num.value));
        return wrap(f.unit ? el("div", { className: "cfg-range" }, num, el("span", { className: "unit" }, f.unit)) : num);
      }
      const max = Math.max(typeof f.max === "function" ? f.max() : f.max, value ?? 0);
      const range = el("input", { type: "range", min, max, step: f.step ?? 1, value });
      range.oninput = () => { num.value = range.value; onChange(parse(range.value)); };
      num.oninput = () => { if (!isNaN(parse(num.value))) { range.value = num.value; onChange(parse(num.value)); } };
      return wrap(el("div", { className: "cfg-range" }, range, num, el("span", { className: "unit" }, f.unit)));
    }
    case "lines": {
      const ta = el("textarea", { rows: 3, value: (value ?? []).join("\n") });
      ta.oninput = () => onChange(ta.value.split("\n").map((s) => s.trim()).filter(Boolean));
      return wrap(ta);
    }
    case "kv": {
      const ta = el("textarea", { rows: 3, value: kvToText(value) });
      ta.oninput = () => onChange(textToKv(ta.value));
      return wrap(ta);
    }
    case "json": {
      const ta = el("textarea", { rows: 5, value: Object.keys(value ?? {}).length ? JSON.stringify(value, null, 2) : "" });
      const err = el("small", { className: "cfg-json-error" });
      ta.oninput = () => {
        try {
          const v = ta.value.trim() ? JSON.parse(ta.value) : {};
          if (typeof v !== "object" || Array.isArray(v)) throw new Error("must be a JSON object");
          err.textContent = "";
          onChange(v);
        } catch (e) {
          err.textContent = `Invalid JSON: ${e.message}`;
        }
      };
      const field = wrap(ta);
      field.append(err);
      return field;
    }
    default: {
      const input = el("input", { value: value ?? "", placeholder: f.placeholder ?? "", disabled: locked });
      input.oninput = () => onChange(input.value);
      return wrap(input);
    }
  }
}

function mountsEl() {
  const list = cfgModel.mounts;
  const box = el("div");
  const render = () => {
    box.replaceChildren(
      ...list.map((m, i) => {
        const loc = el("input", { value: m.location, placeholder: "~/projects" });
        loc.oninput = () => { m.location = loc.value; updateDirty(); };
        const mp = el("input", { value: m.mountPoint ?? "", placeholder: "mount point in VM (same path)" });
        mp.oninput = () => { if (mp.value.trim()) m.mountPoint = mp.value; else delete m.mountPoint; updateDirty(); };
        const w = el("input", { type: "checkbox", checked: !!m.writable });
        w.onchange = () => { m.writable = w.checked; updateDirty(); };
        const rm = el("button", { className: "small ghost", type: "button" }, "Remove");
        rm.onclick = () => { list.splice(i, 1); render(); updateDirty(); };
        return el("div", { className: "cfg-list-row" }, loc, mp, el("label", { className: "check" }, w, " writable"), rm);
      }),
      ...(list.length ? [] : [el("p", { className: "hint" }, "No custom mounts: Colima mounts your home directory (writable) by default.")])
    );
  };
  render();
  const add = el("button", { className: "small secondary", type: "button" }, "Add mount");
  add.onclick = () => { list.push({ location: "", writable: true }); render(); updateDirty(); };
  return el("div", {}, box, add);
}

function provisionEl() {
  const list = cfgModel.provision;
  const box = el("div");
  const render = () => {
    box.replaceChildren(
      ...list.map((p, i) => {
        const mode = el("select", {}, ...["system", "user", "after-boot", "ready"].map((m) => el("option", { value: m, selected: m === p.mode }, m)));
        mode.onchange = () => { p.mode = mode.value; updateDirty(); };
        const script = el("textarea", { rows: 3, value: p.script, placeholder: "apt-get install -y htop" });
        script.oninput = () => { p.script = script.value; updateDirty(); };
        const rm = el("button", { className: "small ghost", type: "button" }, "Remove");
        rm.onclick = () => { list.splice(i, 1); render(); updateDirty(); };
        return el("div", { className: "cfg-list-row provision" }, mode, script, rm);
      }),
      ...(list.length ? [] : [el("p", { className: "hint" }, "Scripts run on every start, so they must be idempotent. system = root, user = your user, after-boot = before runtimes start, ready = after.")])
    );
  };
  render();
  const add = el("button", { className: "small secondary", type: "button" }, "Add script");
  add.onclick = () => { list.push({ mode: "system", script: "" }); render(); updateDirty(); };
  return el("div", {}, box, add);
}

function renderForm() {
  const form = $("#cfg-form");
  if (!cfgModel) { form.replaceChildren(); return; }
  form.replaceChildren(
    ...SECTIONS.map((s) =>
      el("div", { className: "card" }, el("h2", {}, s.title),
        s.custom === "mounts" ? mountsEl()
          : s.custom === "provision" ? provisionEl()
          : el("div", { className: "cfg-grid" }, ...s.fields.map(fieldEl))))
  );
  $("#cfg-raw").value = cfgFile?.raw ?? "";
  $("#cfg-raw").disabled = isNew();
  $("#cfg-path").textContent = isNew() ? "The YAML file is created with the profile." : cfgFile?.path ?? "";
  renderActions();
  updateDirty();
}

function renderActions() {
  const prof = cfgProfiles.find((p) => p.name === cfgProfile);
  const running = prof?.state === "running";
  $("#cfg-new").hidden = !isNew();
  $("#cfg-delete").hidden = isNew();
  $("#cfg-revert").hidden = isNew();
  $("#cfg-save").hidden = isNew();
  $("#cfg-apply").textContent = isNew() ? "Create & Start" : running ? "Save & Restart" : "Save & Start";
  const state = $("#cfg-state");
  state.hidden = isNew() || !prof;
  state.className = `badge ${prof?.state ?? ""}`;
  state.textContent = prof?.state ?? "";
  const busy = !!status?.busy;
  for (const id of ["#cfg-apply", "#cfg-save", "#cfg-delete"]) $(id).disabled = busy;
}

// -------------------------------------------------------------------- load

async function loadColimaProfiles() {
  cfgProfiles = await call("list_profiles", { provider: "colima" }, { quiet: true }).catch(() => []);
  const sel = $("#cfg-profile");
  if (!cfgProfile || (cfgProfile !== NEW && !cfgProfiles.some((p) => p.name === cfgProfile))) {
    const selected = status?.providerId === "colima" ? status.profile : null;
    cfgProfile = cfgProfiles.find((p) => p.name === selected)?.name ?? cfgProfiles[0]?.name ?? NEW;
  }
  sel.replaceChildren(
    ...cfgProfiles.map((p) => el("option", { value: p.name, selected: p.name === cfgProfile }, p.name)),
    el("option", { value: NEW, selected: isNew() }, "New profile…")
  );
}

async function loadConfig() {
  if (isNew()) {
    cfgFile = null;
    cfgModel = await call("colima_config_defaults");
    cfgModel.cpu = Math.min(4, cfgHost.cpus);
    cfgModel.memory = 8;
    cfgModel.disk = 100;
  } else {
    try {
      cfgFile = await call("colima_config_get", { profile: cfgProfile }, { quiet: true });
      cfgModel = structuredClone(cfgFile.config);
    } catch (e) {
      cfgFile = null;
      cfgModel = null;
      $("#cfg-form").replaceChildren(el("div", { className: "card empty" }, String(e)));
      return;
    }
  }
  renderForm();
}

/** Called by app.js whenever the Colima view is shown. */
window.loadColimaView = async () => {
  if (!cfgLoaded) {
    cfgHost = await call("host_info", undefined, { quiet: true }).catch(() => cfgHost);
  }
  await loadColimaProfiles();
  if (!cfgLoaded || (!dirtySettings() && !dirtyYaml() && !isNew())) {
    cfgLoaded = true;
    await loadConfig();
  } else {
    renderActions();
  }
};

$("#cfg-profile").onchange = async (e) => {
  if ((dirtySettings() || dirtyYaml()) && !(await confirmAction("Discard unsaved changes?"))) {
    e.target.value = cfgProfile;
    return;
  }
  cfgProfile = e.target.value;
  if (isNew()) showTab("colima-settings");
  await loadConfig();
};

$("#cfg-raw").addEventListener("input", updateDirty);
document.querySelectorAll('[data-group="colima"] .tab').forEach((t) => t.addEventListener("click", updateDirty));

// ------------------------------------------------------------------ actions

async function save() {
  const yamlTab = $("#colima-yaml").classList.contains("active");
  cfgFile = yamlTab
    ? await call("colima_config_save_raw", { profile: cfgProfile, raw: $("#cfg-raw").value })
    : await call("colima_config_save", { profile: cfgProfile, config: cfgModel });
  cfgModel = structuredClone(cfgFile.config);
  renderForm();
}

$("#cfg-save").onclick = async () => {
  await save();
  toast(`Saved ${cfgProfile}. Restart the profile to apply.`);
};

$("#cfg-revert").onclick = loadConfig;

$("#cfg-apply").onclick = async () => {
  $("#activity").open = true;
  if (isNew()) {
    const name = $("#cfg-new-name").value.trim();
    if (!name) { $("#cfg-new-name").focus(); return toast("Name the new profile", true); }
    try {
      await call("colima_create", { profile: name, config: cfgModel, taskId: newTask("colima") });
      toast(`Created ${name}`);
      cfgProfile = name;
      $("#cfg-new-name").value = "";
    } finally {
      await loadColimaProfiles();
      await loadConfig();
      loadProfiles();
    }
    return;
  }
  const prof = cfgProfiles.find((p) => p.name === cfgProfile);
  const running = prof?.state === "running";
  if (running && !(await confirmAction(`Restart ${cfgProfile} to apply the settings? Running containers will stop.`, "Restart Colima"))) return;
  await save();
  try {
    await call("colima_apply", { profile: cfgProfile, taskId: newTask("colima") });
    toast(`${cfgProfile} ${running ? "restarted" : "started"} with the new settings`);
  } finally {
    await loadColimaProfiles();
    renderActions();
    loadProfiles();
  }
};

$("#cfg-delete").onclick = async () => {
  const name = cfgProfile;
  if (!(await confirmAction(`Delete the Colima profile “${name}”? Its VM is removed.`, "Delete profile"))) return;
  const deleteData = await confirmAction(
    `Also delete ${name}'s container data (images, containers, volumes)? Choose No to keep it for a recreated profile.`,
    "Delete container data");
  $("#activity").open = true;
  try {
    await call("colima_delete", { profile: name, deleteData, taskId: newTask("colima") });
    toast(`Deleted ${name}`);
    cfgProfile = null;
  } finally {
    await loadColimaProfiles();
    await loadConfig();
    loadProfiles();
  }
};

// Keep busy state / profile states current.
listen("status", () => {
  if ($("#view-colima").classList.contains("active")) renderActions();
});

// -------------------------------------------------------------------- data

function fmtBytes(n) {
  if (n == null) return "—";
  if (n === 0) return "0";
  return bytes(n);
}

async function loadData() {
  const body = $("#data-body");
  body.replaceChildren(el("tr", {}, el("td", { colSpan: 5, className: "empty" }, "Calculating…")));
  let usage;
  try {
    usage = await call("colima_data_usage", undefined, { quiet: true });
  } catch (e) {
    body.replaceChildren(el("tr", {}, el("td", { colSpan: 5, className: "empty" }, String(e))));
    return;
  }
  body.replaceChildren(
    ...(usage.profiles.length
      ? usage.profiles.map((p) => {
          const del = el("button", { className: "small danger" }, "Delete with data");
          del.onclick = async () => {
            if (!(await confirmAction(
              `Delete the Colima profile “${p.name}” together with its images, containers and volumes? This can't be undone.`,
              "Delete profile and data"))) return;
            del.disabled = true;
            $("#activity").open = true;
            try {
              await call("colima_delete", { profile: p.name, deleteData: true, taskId: newTask("colima") });
              toast(`Deleted ${p.name} and its data`);
            } finally {
              afterDelete();
            }
          };
          return el("tr", {},
            el("td", {}, p.name),
            el("td", {}, el("span", { className: `badge ${p.state}` }, p.state)),
            el("td", {}, fmtBytes(p.vmBytes)),
            el("td", {}, fmtBytes(p.dataBytes)),
            el("td", { className: "actions" }, del));
        })
      : [el("tr", {}, el("td", { colSpan: 5, className: "empty" }, "No Colima profiles"))])
  );
  $("#cache-info").textContent = usage.cachePath
    ? `${fmtBytes(usage.cacheBytes)} in ${usage.cachePath}`
    : "No cache directory found.";
}

async function afterDelete() {
  cfgProfile = null;
  await loadColimaProfiles();
  await loadConfig();
  await loadData();
  loadProfiles();
}

$("#data-refresh").onclick = loadData;

$("#cache-clear").onclick = async () => {
  if (!(await confirmAction("Clear Colima's download cache?", "Clear cache"))) return;
  $("#activity").open = true;
  try {
    await call("colima_prune", { taskId: newTask("colima") });
    toast("Download cache cleared");
  } finally {
    loadData();
  }
};

$("#nuke-confirm").addEventListener("input", (e) => {
  $("#nuke").disabled = e.target.value.trim() !== "DELETE ALL";
});

$("#nuke").onclick = async () => {
  const names = cfgProfiles.map((p) => p.name).join(", ") || "(none)";
  if (!(await confirmAction(`Delete ALL Colima profiles and their data?\n\n${names}`, "Delete everything"))) return;
  $("#nuke").disabled = true;
  $("#nuke-confirm").value = "";
  $("#activity").open = true;
  try {
    await call("colima_delete_all", { clearCache: $("#nuke-cache").checked, taskId: newTask("colima") });
    toast("All Colima data deleted");
  } finally {
    afterDelete();
  }
};

// The Data tab has no Save bar; it loads when opened.
document.querySelectorAll('[data-group="colima"] .tab').forEach((t) =>
  t.addEventListener("click", () => {
    const data = t.dataset.tab === "colima-data";
    $("#cfg-actions").hidden = data;
    $("#cfg-profile").disabled = data;
    if (data) loadData();
  })
);
