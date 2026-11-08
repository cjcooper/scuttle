// Compose view: registered docker compose stacks (start with the app, stop
// on quit) and the stack editor dialog. Shares helpers with app.js.

let stacks = [];
const openStacks = new Set(); // ids whose services row is expanded
let csEditing = null;         // stack being edited (null = new)
const csDialog = $("#compose-dialog");

const ON_QUIT = { stop: "Stop", down: "Down", leave: "Leave running" };

// ------------------------------------------------------------------- list

function stackStatus(services) {
  if (!services) return el("span", { className: "badge" }, "…");
  if (services.error) return el("span", { className: "badge exited", title: services.error }, "error");
  const total = services.length;
  const running = services.filter((s) => s.state === "running").length;
  if (!total) return el("span", { className: "badge" }, "not created");
  const cls = running === total ? "running" : running ? "partial" : "exited";
  return el("span", { className: `badge ${cls}` }, running === total ? `running (${total})` : `${running}/${total} running`);
}

function servicesTable(services) {
  if (!services || services.error || !services.length) {
    return el("p", { className: "hint" }, services?.error ?? "No containers for this stack yet.");
  }
  return el("table", {},
    el("thead", {}, el("tr", {}, ...["Service", "State", "Status", "Ports"].map((h) => el("th", {}, h)))),
    el("tbody", {}, ...services.map((s) =>
      el("tr", {},
        el("td", {}, s.service),
        el("td", {}, el("span", { className: `badge ${s.state}` }, s.state + (s.health ? ` · ${s.health}` : ""))),
        el("td", {}, s.status),
        el("td", { className: "mono" }, s.ports)))));
}

async function composeAction(stack, action, button) {
  if (action === "down" && !(await confirmAction(`Take down “${stack.name}”? Its containers and networks are removed (volumes are kept).`, "Compose down", "Down"))) return;
  if (button) button.disabled = true;
  $("#activity").open = true;
  try {
    await call("compose_action", { id: stack.id, action, taskId: newTask("compose") });
    toast(`${stack.name}: ${action} done`);
  } finally {
    if (button) button.disabled = false;
    loadStacks();
  }
}

function renderStacks(statuses = {}) {
  const body = $("#compose-body");
  if (!stacks.length) {
    body.replaceChildren(el("tr", {}, el("td", { colSpan: 5, className: "empty" }, "No stacks yet. Add one, or paste a docker compose command.")));
    return;
  }
  const rows = [];
  for (const st of stacks) {
    const services = statuses[st.id];
    const running = Array.isArray(services) && services.some((s) => s.state === "running");
    const btn = (label, action, cls = "secondary") => {
      const b = el("button", { className: `small ${cls}` }, label);
      b.onclick = () => composeAction(st, action, b);
      return b;
    };
    const logs = el("button", { className: "small ghost" }, "Logs");
    logs.onclick = () => call("compose_logs", { id: st.id });
    const edit = el("button", { className: "small ghost" }, "Edit");
    edit.onclick = () => openStackDialog(st);
    const auto = el("input", { type: "checkbox", checked: st.autostart, title: "Start with this app" });
    auto.onchange = async () => {
      stacks = await call("compose_save", { stack: { ...stripView(st), autostart: auto.checked } });
      renderStacks(statuses);
    };
    const name = el("span", { className: "stack-name", title: st.command }, st.name);
    name.onclick = () => {
      openStacks.has(st.id) ? openStacks.delete(st.id) : openStacks.add(st.id);
      renderStacks(statuses);
    };
    const open = openStacks.has(st.id);
    rows.push(el("tr", { className: open ? "open" : "" },
      el("td", {}, name, el("span", { className: "stack-dir" }, st.directory)),
      el("td", {}, stackStatus(services)),
      el("td", {}, auto),
      el("td", {}, ON_QUIT[st.onQuit] ?? st.onQuit),
      el("td", { className: "actions" },
        running ? btn("Stop", "stop") : btn("Up", "up", ""),
        ...(running ? [btn("Restart", "restart")] : []),
        btn("Down", "down", "ghost"),
        logs, edit)));
    if (open) rows.push(el("tr", { className: "services" }, el("td", { colSpan: 5 }, servicesTable(services))));
  }
  body.replaceChildren(...rows);
}

const stripView = (st) => {
  const { command, ...stack } = st;
  return stack;
};

async function loadStacks() {
  stacks = await call("compose_list", undefined, { quiet: true }).catch(() => []);
  renderStacks();
  if (!engineUp()) return;
  const statuses = {};
  await Promise.all(stacks.map(async (st) => {
    try {
      statuses[st.id] = await call("compose_services", { id: st.id }, { quiet: true });
    } catch (e) {
      statuses[st.id] = { error: String(e) };
    }
  }));
  renderStacks(statuses);
}

$("#compose-refresh").onclick = loadStacks;
$("#compose-add").onclick = () => openStackDialog(null);
listen("compose", () => {
  if ($("#view-compose").classList.contains("active")) loadStacks();
});

// ----------------------------------------------------------------- editor

const csList = (v) => v.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);

function readStack() {
  return {
    id: csEditing?.id ?? "",
    name: $("#cs-name").value.trim(),
    directory: $("#cs-dir").value.trim(),
    files: $("#cs-files").value.split("\n").map((s) => s.trim()).filter(Boolean),
    projectName: $("#cs-project").value.trim(),
    profiles: csList($("#cs-profiles").value),
    envFiles: csList($("#cs-envfiles").value),
    env: $("#cs-env").value.split("\n").map((s) => s.trim()).filter(Boolean),
    hostIds: $("#cs-hostids").checked,
    upArgs: shellWords($("#cs-upargs").value),
    autostart: $("#cs-autostart").checked,
    onQuit: $("#cs-onquit").value,
  };
}

function fillStack(st) {
  $("#cs-name").value = st.name ?? "";
  $("#cs-dir").value = st.directory ?? "";
  $("#cs-files").value = (st.files ?? []).join("\n");
  $("#cs-project").value = st.projectName ?? "";
  $("#cs-profiles").value = (st.profiles ?? []).join(", ");
  $("#cs-envfiles").value = (st.envFiles ?? []).join(", ");
  $("#cs-env").value = (st.env ?? []).join("\n");
  $("#cs-hostids").checked = st.hostIds ?? true;
  $("#cs-upargs").value = (st.upArgs ?? []).map(quoteWord).join(" ");
  $("#cs-autostart").checked = !!st.autostart;
  $("#cs-onquit").value = st.onQuit ?? "stop";
  updateStackPreview();
}

async function updateStackPreview() {
  $("#cs-preview").textContent = await call("compose_preview", { stack: readStack() }, { quiet: true }).catch(String);
}

async function openStackDialog(st) {
  csEditing = st ? stripView(st) : null;
  $("#cs-title").textContent = st ? `Edit ${st.name}` : "Add compose stack";
  $("#cs-delete").hidden = !st;
  $("#cs-error").textContent = "";
  $("#cs-paste").value = "";
  fillStack(csEditing ?? { hostIds: true, onQuit: "stop" });
  const ids = await call("compose_host_ids", undefined, { quiet: true }).catch(() => null);
  $("#cs-hostids-label").textContent = ids ? `Set UID=${ids[0]} and GID=${ids[1]} (my user)` : "Set UID and GID to my user";
  csDialog.showModal();
}

$("#cs-form").addEventListener("input", updateStackPreview);
$("#cs-form").addEventListener("change", updateStackPreview);
$("#cs-close").onclick = () => csDialog.close();
$("#cs-cancel").onclick = () => csDialog.close();

$("#cs-pick-dir").onclick = async () => {
  const dir = await dialog.open({ directory: true, title: "Compose project directory", defaultPath: $("#cs-dir").value || undefined });
  if (!dir) return;
  $("#cs-dir").value = dir;
  if (!$("#cs-name").value.trim()) $("#cs-name").value = dir.split(/[\\/]/).filter(Boolean).pop() ?? "";
  updateStackPreview();
};

$("#cs-pick-files").onclick = async () => {
  const dir = $("#cs-dir").value.trim();
  const picked = await dialog.open({
    multiple: true, title: "Compose files", defaultPath: dir || undefined,
    filters: [{ name: "Compose files", extensions: ["yml", "yaml"] }],
  });
  if (!picked) return;
  const files = (Array.isArray(picked) ? picked : [picked]).map((f) =>
    dir && f.startsWith(dir.replace(/\/$/, "") + "/") ? f.slice(dir.replace(/\/$/, "").length + 1) : f);
  const existing = $("#cs-files").value.split("\n").map((s) => s.trim()).filter(Boolean);
  $("#cs-files").value = [...existing, ...files.filter((f) => !existing.includes(f))].join("\n");
  updateStackPreview();
};

/** Parse `[ENV=…] docker compose|docker-compose [global opts] up [args]`. */
function parseComposeCommand(text) {
  let w = shellWords(text.replace(/\\\n/g, " "));
  const out = { env: [], hostIds: false, files: [], profiles: [], envFiles: [], projectName: "", upArgs: [], directory: null };
  while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(w[0] ?? "")) {
    const [k, ...rest] = w.shift().split("=");
    const v = rest.join("=");
    if ((k === "UID" && /^\$\{?UID\}?$/.test(v)) || (k === "GID" && /^\$\{?GID\}?$/.test(v))) out.hostIds = true;
    else out.env.push(`${k}=${v}`);
  }
  if (w[0] === "docker" && w[1] === "compose") w = w.slice(2);
  else if (w[0] === "docker-compose" || w[0]?.endsWith("/docker-compose")) w = w.slice(1);
  else return null;
  for (let i = 0; i < w.length; i++) {
    let a = w[i], val = null;
    const eq = a.startsWith("--") ? a.indexOf("=") : -1;
    if (eq > 0) { val = a.slice(eq + 1); a = a.slice(0, eq); }
    const take = () => val ?? w[++i];
    if (a === "-f" || a === "--file") out.files.push(take());
    else if (a === "-p" || a === "--project-name") out.projectName = take();
    else if (a === "--profile") out.profiles.push(take());
    else if (a === "--env-file") out.envFiles.push(take());
    else if (a === "--project-directory") out.directory = take();
    else if (!a.startsWith("-")) {
      // The subcommand; keep `up` arguments (detach is always added).
      if (a === "up") out.upArgs = w.slice(i + 1).filter((x) => x !== "-d" && x !== "--detach");
      break;
    }
  }
  return out;
}

$("#cs-parse").onclick = async () => {
  const text = $("#cs-paste").value.trim();
  if (!text) return;
  const expanded = await call("compose_expand", { text }, { quiet: true }).catch(() => text);
  const parsed = parseComposeCommand(expanded);
  if (!parsed) return toast(`Not a docker compose command: ${expanded}`, true);
  const cur = readStack();
  fillStack({
    ...cur,
    files: parsed.files,
    projectName: parsed.projectName,
    profiles: parsed.profiles,
    envFiles: parsed.envFiles,
    env: parsed.env,
    hostIds: parsed.hostIds,
    upArgs: parsed.upArgs,
    directory: parsed.directory ?? cur.directory,
  });
  toast(expanded === text ? "Form filled from command" : `Expanded to: ${expanded}`);
};
$("#cs-paste").addEventListener("keydown", (e) => { if (e.key === "Enter") $("#cs-parse").click(); });

$("#cs-save").onclick = async () => {
  try {
    stacks = await invoke("compose_save", { stack: readStack() });
    csDialog.close();
    toast("Stack saved");
    loadStacks();
  } catch (e) {
    $("#cs-error").textContent = String(e);
  }
};

$("#cs-delete").onclick = async () => {
  if (!csEditing) return;
  if (!(await confirmAction(`Remove “${csEditing.name}” from the app? Its containers are not touched.`, "Remove stack", "Remove"))) return;
  stacks = await call("compose_delete", { id: csEditing.id });
  csDialog.close();
  loadStacks();
};
