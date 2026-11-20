// Resources view: CPU, memory, network and disk graphs for running
// containers, grouped by compose project, with drill-down
//   All containers → compose project → container.
// Samples come from the backend sampler (stats.rs), which runs while this
// page polls. Shares helpers with app.js and charts.js.

const RES = {
  since: 0,
  samples: [],          // [{ seq, t, containers: [...], byId: Map }]
  known: new Map(),     // id → member (kept after a container disappears)
  members: [],          // latest `container ls -a`
  ncpu: 0,
  memTotal: 0,
  intervalMs: 3000,
  error: null,
  windowMs: 5 * 60_000,
  view: { kind: "overview" },
  timer: null,
  charts: {},
  slots: new Map(),     // context → Map(entity key → slot)
  inspect: null,        // container detail: inspect JSON
  stacks: [],           // registered compose stacks, for group actions
};
const RETAIN_MS = 15 * 60_000;
const MAX_SLOTS = 8;

// ------------------------------------------------------------------ polling

async function resPoll() {
  let b;
  try {
    b = await invoke("stats_poll", { since: RES.since });
  } catch (e) {
    RES.error = String(e);
    return renderResources();
  }
  if (b.reset) RES.samples = [];
  for (const s of b.samples) {
    s.byId = new Map(s.containers.map((c) => [c.id, c]));
    RES.samples.push(s);
  }
  const cutoff = Date.now() - RETAIN_MS;
  while (RES.samples.length && RES.samples[0].t < cutoff) RES.samples.shift();
  RES.since = b.seq;
  RES.members = b.members;
  for (const m of b.members) RES.known.set(m.id, m);
  RES.ncpu = b.ncpu;
  RES.memTotal = b.memTotal;
  RES.intervalMs = b.intervalMs || 3000;
  RES.error = b.error;
  renderResources();
}

function resStart() {
  if (RES.timer) return;
  resPoll();
  RES.timer = setInterval(() => { if (!document.hidden) resPoll(); }, 2000);
}
function resStop() {
  clearInterval(RES.timer);
  RES.timer = null;
}
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && RES.timer) resPoll();
});

/** Called by showView in app.js. */
function resourcesShown(active) {
  if (active) { resStart(); loadResStacks(); } else resStop();
}

async function loadResStacks() {
  RES.stacks = await call("compose_list", undefined, { quiet: true }).catch(() => []);
}

// ----------------------------------------------------------------- model

const memberOf = (id) => RES.known.get(id) ?? { id, name: id.slice(0, 12), project: "", service: "", state: "" };
const groupKey = (m) => (m.project ? `p:${m.project}` : `c:${m.id}`);
/** Container name without the `project-` prefix compose adds. */
const shortName = (m) => (m.project && m.name.startsWith(m.project + "-") ? m.name.slice(m.project.length + 1) : m.name);

/** Fixed color slot per entity within a context, in first-seen order; never re-ranked. */
function slotFor(context, key) {
  let map = RES.slots.get(context);
  if (!map) RES.slots.set(context, (map = new Map()));
  if (!map.has(key)) map.set(key, map.size < MAX_SLOTS ? map.size : null);
  return map.get(key);
}

function windowSamples() {
  const t0 = Date.now() - RES.windowMs;
  // Keep one sample before the window so lines reach its left edge.
  let i = RES.samples.findIndex((s) => s.t >= t0);
  if (i < 0) i = RES.samples.length;
  return RES.samples.slice(Math.max(0, i - 1));
}

const sum = (cs, f) => (cs.length ? cs.reduce((a, c) => a + (c[f] ?? 0), 0) : null);

/** values[i] = f(containers in samples[i] that belong to `ids`), null when none were running. */
function seriesValues(samples, ids, field) {
  return samples.map((s) => {
    const cs = [];
    for (const id of ids) { const c = s.byId.get(id); if (c) cs.push(c); }
    return sum(cs, field);
  });
}

/**
 * Entities (groups or containers) for a context, as stacked series with
 * fixed slots; anything past the 8 slots folds into "Other".
 */
function entitySeries(context, samples, entities, field) {
  // Assign slots alphabetically the first time a context is seen.
  [...entities].sort((a, b) => a.label.localeCompare(b.label)).forEach((e) => slotFor(context, e.key));
  const withSlot = entities.filter((e) => slotFor(context, e.key) != null)
    .sort((a, b) => slotFor(context, a.key) - slotFor(context, b.key));
  const other = entities.filter((e) => slotFor(context, e.key) == null);
  const series = withSlot.map((e) => ({
    key: e.key, label: e.label, slot: slotFor(context, e.key), values: seriesValues(samples, e.ids, field),
  }));
  if (other.length) {
    series.push({ key: "other", label: `Other (${other.length})`, slot: null,
      values: seriesValues(samples, other.flatMap((e) => e.ids), field) });
  }
  return series;
}

/** Ids seen in the window, grouped into overview entities. */
function overviewEntities(samples) {
  const groups = new Map();
  for (const s of samples) for (const c of s.containers) {
    const m = memberOf(c.id);
    const key = groupKey(m);
    if (!groups.has(key)) groups.set(key, { key, label: m.project || m.name, project: m.project, ids: new Set() });
    groups.get(key).ids.add(c.id);
  }
  return [...groups.values()].map((g) => ({ ...g, ids: [...g.ids] }));
}

function projectEntities(samples, project) {
  const ids = new Set();
  for (const s of samples) for (const c of s.containers) if (memberOf(c.id).project === project) ids.add(c.id);
  for (const m of RES.members) if (m.project === project && m.state === "running") ids.add(m.id);
  return [...ids].map((id) => ({ key: `c:${id}`, label: shortName(memberOf(id)), ids: [id] }));
}

// ------------------------------------------------------------- navigation

function resGo(view) {
  RES.view = view;
  RES.charts = {};
  RES.inspect = null;
  $("#res-charts").replaceChildren();
  if (view.kind === "container") loadInspect(view.id);
  if (!$("#view-resources").classList.contains("active")) showView("resources");
  renderResources();
  $("main").scrollTop = 0;
}
window.resGo = resGo;

function crumbs() {
  const v = RES.view;
  const link = (label, view) => {
    const a = el("button", { className: "crumb" }, label);
    a.onclick = () => resGo(view);
    return a;
  };
  const parts = [link("All containers", { kind: "overview" })];
  if (v.kind === "group") parts.push(el("span", { className: "crumb current" }, v.project));
  if (v.kind === "container") {
    const m = memberOf(v.id);
    if (m.project) parts.push(link(m.project, { kind: "group", project: m.project }));
    parts.push(el("span", { className: "crumb current" }, m.project ? shortName(m) : m.name));
  }
  return parts.flatMap((p, i) => (i ? [el("span", { className: "crumb-sep" }, "›"), p] : [p]));
}

$("#res-window").onchange = (e) => { RES.windowMs = +e.target.value; renderResources(); };

// ---------------------------------------------------------------- render

function renderResources() {
  if (!$("#view-resources").classList.contains("active")) return;
  $("#res-crumbs").replaceChildren(...crumbs());
  const note = $("#res-note");
  const samples = windowSamples();
  if (!engineUp()) {
    note.textContent = "Engine not running.";
  } else if (RES.error && !samples.length) {
    note.textContent = RES.error;
  } else if (!RES.samples.length) {
    note.textContent = "Collecting the first sample…";
  } else {
    note.textContent = "";
  }
  note.hidden = !note.textContent;
  const v = RES.view;
  $("#res-table").hidden = v.kind === "container";
  if (v.kind !== "container") $("#res-info").__inspect = undefined;
  if (v.kind === "overview") renderOverview(samples);
  else if (v.kind === "group") renderGroup(samples, v.project);
  else renderContainer(samples, v.id);
}

function chart(name, opts) {
  return (RES.charts[name] ??= new TimeChart($("#res-charts"), opts));
}

function chartData(samples, series, extra = {}) {
  const t1 = Date.now();
  return { times: samples.map((s) => s.t), series, t0: t1 - RES.windowMs, t1, gapMs: RES.intervalMs * 3.5, ...extra };
}

/** Two-line chart (e.g. received/sent) of summed rates for `ids`. */
function ioSeries(samples, ids, [a, b], [la, lb]) {
  return [
    { key: a, label: la, short: la === "Received" ? "↓" : "read", slot: 0, values: seriesValues(samples, ids, a) },
    { key: b, label: lb, short: lb === "Sent" ? "↑" : "write", slot: 1, values: seriesValues(samples, ids, b) },
  ];
}

function tile(label, value, sub = "") {
  return el("div", { className: "tile" }, el("small", {}, label), el("b", {}, value), el("small", {}, sub));
}

const pctOf = (v, total) => (total ? ` · ${((v / total) * 100).toFixed(v / total < 0.1 ? 1 : 0)}%` : "");

function renderTiles(latest, ids, { showCount = true, memLimit = null } = {}) {
  const cs = latest ? ids.map((id) => latest.byId.get(id)).filter(Boolean) : [];
  const cpu = sum(cs, "cpu") ?? 0, mem = sum(cs, "mem") ?? 0;
  const cores = cpu / 100;
  const memOf = memLimit ?? RES.memTotal;
  const tiles = [
    tile("CPU", `${cores >= 10 ? cores.toFixed(1) : cores.toFixed(2)} CPUs`, RES.ncpu ? `of ${RES.ncpu}${pctOf(cores, RES.ncpu)}` : ""),
    tile("Memory", fmtBytes(mem), memOf ? `of ${fmtBytes(memOf)}${memLimit ? " limit" : ""}${pctOf(mem, memOf)}` : ""),
    tile("Network", `↓ ${fmtValue(sum(cs, "netRx") ?? 0, "rate")}`, `↑ ${fmtValue(sum(cs, "netTx") ?? 0, "rate")}`),
    tile("Disk I/O", `${fmtValue(sum(cs, "blkRead") ?? 0, "rate")} read`, `${fmtValue(sum(cs, "blkWrite") ?? 0, "rate")} write`),
  ];
  if (showCount) tiles.unshift(tile("Running", String(cs.length), "containers"));
  $("#res-tiles").replaceChildren(...tiles);
}

// Table rows: one per entity with sparklines and latest values.
function usageRow({ name, sub, slot, ids, samples, state, onOpen, title }) {
  const latest = samples.at(-1);
  const cs = latest ? ids.map((id) => latest.byId.get(id)).filter(Boolean) : [];
  const val = (f) => sum(cs, f);
  const spark = (f, unit) => {
    const vs = seriesValues(samples.slice(-60), ids, f);
    return el("div", { className: "spark-cell" }, sparkline(vs), el("span", {}, cs.length ? fmtValue(val(f), unit) : "—"));
  };
  const limit = cs.length === 1 && cs[0].memLimit && cs[0].memLimit < RES.memTotal * 0.99 ? cs[0].memLimit : null;
  const nameCell = el("td", { className: "res-name" },
    el("i", { className: "swatch", style: slot === undefined ? "visibility:hidden" : `background:${seriesColor(slot)}` }),
    el("span", {}, el("b", { title: title ?? name }, name), sub ? el("small", {}, sub) : ""));
  const tr = el("tr", { className: onOpen ? "clickable" : "" },
    nameCell,
    el("td", {}, state),
    el("td", {}, spark("cpu", "pct")),
    el("td", {}, spark("mem", "bytes"), limit ? el("small", { className: "muted" }, `limit ${fmtBytes(limit)}`) : ""),
    el("td", { className: "num" }, cs.length ? `↓ ${fmtValue(val("netRx"), "rate")}` : "—", el("br"), cs.length ? `↑ ${fmtValue(val("netTx"), "rate")}` : ""),
    el("td", { className: "num" }, cs.length ? `r ${fmtValue(val("blkRead"), "rate")}` : "—", el("br"), cs.length ? `w ${fmtValue(val("blkWrite"), "rate")}` : ""),
    el("td", { className: "num" }, cs.length ? String(val("pids")) : "—"),
    el("td", { className: "actions" }, onOpen ? el("span", { className: "chev" }, "›") : ""));
  if (onOpen) tr.onclick = (e) => { if (!e.target.closest("button")) onOpen(); };
  return tr;
}

function renderTable(rows, emptyMsg) {
  $("#res-body").replaceChildren(...(rows.length ? rows
    : [el("tr", {}, el("td", { colSpan: 8, className: "empty" }, emptyMsg))]));
}

const stateBadge = (state, text = state) => el("span", { className: `badge ${state}` }, text);

// ------------------------------------------------------------- overview

function renderOverview(samples) {
  $("#res-title").textContent = "All containers";
  $("#res-info").replaceChildren();
  $("#res-actions").replaceChildren();
  const ents = overviewEntities(samples);
  const latest = samples.at(-1);
  renderTiles(latest, latest ? latest.containers.map((c) => c.id) : []);

  const cpu = chart("cpu", { title: "CPU", subtitle: "100% = 1 CPU", unit: "pct", stacked: true });
  const mem = chart("mem", { title: "Memory", unit: "bytes", stacked: true });
  const net = chart("net", { title: "Network", unit: "rate" });
  const disk = chart("disk", { title: "Disk I/O", unit: "rate" });
  const allIds = ents.flatMap((e) => e.ids);
  cpu.update(chartData(samples, entitySeries("overview", samples, ents, "cpu")));
  mem.update(chartData(samples, entitySeries("overview", samples, ents, "mem")));
  net.update(chartData(samples, ioSeries(samples, allIds, ["netRx", "netTx"], ["Received", "Sent"])));
  disk.update(chartData(samples, ioSeries(samples, allIds, ["blkRead", "blkWrite"], ["Read", "Written"])));

  // Table: projects first, then standalone containers; only those running now
  // or seen in the window. Stopped ones are counted in the hint.
  const running = new Set(latest?.containers.map((c) => c.id) ?? []);
  const byProject = new Map();
  for (const m of RES.members) if (m.project) {
    if (!byProject.has(m.project)) byProject.set(m.project, []);
    byProject.get(m.project).push(m);
  }
  const rows = ents
    .sort((a, b) => (!!b.project - !!a.project) || a.label.localeCompare(b.label))
    .map((e) => {
      const slot = slotFor("overview", e.key);
      if (e.project) {
        const all = byProject.get(e.project) ?? [];
        const up = all.filter((m) => m.state === "running").length;
        return usageRow({ name: e.project, sub: "compose project", slot, ids: e.ids, samples,
          state: stateBadge(up === all.length && up ? "running" : up ? "partial" : "exited", `${up}/${all.length || e.ids.length} running`),
          onOpen: () => resGo({ kind: "group", project: e.project }) });
      }
      const m = memberOf(e.ids[0]);
      return usageRow({ name: m.name, sub: m.image, slot, ids: e.ids, samples,
        state: stateBadge(running.has(m.id) ? "running" : m.state || "exited", running.has(m.id) ? "running" : m.state || "gone"),
        onOpen: () => resGo({ kind: "container", id: m.id }) });
    });
  renderTable(rows, engineUp() ? "No running containers" : "Engine not running");
  const stopped = RES.members.filter((m) => m.state !== "running" && !m.project).length;
  $("#res-foot").textContent = stopped ? `${stopped} stopped standalone container(s) not shown.` : "";
}

// ---------------------------------------------------------------- group

function stackForProject(project) {
  const dirs = new Set(RES.members.filter((m) => m.project === project).map((m) => m.workingDir).filter(Boolean));
  const norm = (s) => s.toLowerCase().replace(/[^a-z0-9_-]/g, "");
  return RES.stacks.find((st) =>
    (st.projectName ? st.projectName === project : dirs.has(st.directory.replace(/\/$/, "")) || norm(st.directory.split(/[\\/]/).filter(Boolean).pop() ?? "") === project));
}

function renderGroup(samples, project) {
  $("#res-title").textContent = project;
  const members = RES.members.filter((m) => m.project === project);
  const ents = projectEntities(samples, project);
  const ids = ents.flatMap((e) => e.ids);
  const latest = samples.at(-1);
  renderTiles(latest, ids);
  const dir = members.find((m) => m.workingDir)?.workingDir;
  const stack = stackForProject(project);
  $("#res-info").replaceChildren(
    ...(dir ? [el("span", { className: "mono" }, dir)] : []),
    el("span", {}, stack ? `Registered stack “${stack.name}”` : "Not registered in Compose"));

  const actions = [];
  if (stack) {
    const running = members.some((m) => m.state === "running");
    const btn = (label, action, cls = "secondary") => {
      const b = el("button", { className: `small ${cls}` }, label);
      b.onclick = () => composeAction(stack, action, b);
      return b;
    };
    const logs = el("button", { className: "small ghost" }, "Logs");
    logs.onclick = () => call("compose_logs", { id: stack.id });
    actions.push(...(running ? [btn("Restart", "restart"), btn("Stop", "stop")] : [btn("Up", "up", "")]), logs);
  }
  $("#res-actions").replaceChildren(...actions);

  const ctx = `group:${project}`;
  chart("cpu", { title: "CPU by service", subtitle: "100% = 1 CPU", unit: "pct", stacked: true })
    .update(chartData(samples, entitySeries(ctx, samples, ents, "cpu")));
  chart("mem", { title: "Memory by service", unit: "bytes", stacked: true })
    .update(chartData(samples, entitySeries(ctx, samples, ents, "mem")));
  chart("net", { title: "Network", unit: "rate" })
    .update(chartData(samples, ioSeries(samples, ids, ["netRx", "netTx"], ["Received", "Sent"])));
  chart("disk", { title: "Disk I/O", unit: "rate" })
    .update(chartData(samples, ioSeries(samples, ids, ["blkRead", "blkWrite"], ["Read", "Written"])));

  const known = new Map(members.map((m) => [m.id, m]));
  for (const id of ids) if (!known.has(id)) known.set(id, memberOf(id));
  const rows = [...known.values()]
    .sort((a, b) => shortName(a).localeCompare(shortName(b)))
    .map((m) => {
      const slot = ids.includes(m.id) ? slotFor(ctx, `c:${m.id}`) : undefined;
      return usageRow({ name: shortName(m), sub: m.image, slot, ids: [m.id], samples, title: m.name,
        state: stateBadge(m.state || "exited", m.status || m.state || "gone"),
        onOpen: () => resGo({ kind: "container", id: m.id }) });
    });
  renderTable(rows, "No containers in this project");
  $("#res-foot").textContent = "";
}

// ------------------------------------------------------------ container

async function loadInspect(id) {
  try {
    RES.inspect = await call("container_inspect", { id }, { quiet: true });
  } catch (e) {
    RES.inspect = { error: String(e) };
  }
  renderResources();
}

function renderContainer(samples, id) {
  const m = memberOf(id);
  $("#res-title").textContent = m.name;
  const latest = samples.at(-1);
  const c = latest?.byId.get(id);
  const memLimit = c?.memLimit && c.memLimit < RES.memTotal * 0.99 ? c.memLimit : null;
  renderTiles(latest, [id], { showCount: false, memLimit });
  const running = m.state === "running";

  const btn = (label, action, cls = "secondary") => {
    const b = el("button", { className: `small ${cls}` }, label);
    b.onclick = async () => {
      b.disabled = true;
      try { await call("container_action", { id, action }); toast(`${m.name}: ${action} done`); }
      finally { b.disabled = false; loadInspect(id); resPoll(); }
    };
    return b;
  };
  const shell = el("button", { className: "small", title: `docker exec as ${userLabel()}` }, "Shell");
  shell.onclick = () => call("open_shell", { container: m.name || id });
  const logs = el("button", { className: "small ghost" }, "Logs");
  logs.onclick = () => call("container_logs", { id, name: m.name });
  $("#res-actions").replaceChildren(...(running ? [shell, btn("Restart", "restart"), btn("Stop", "stop")] : [btn("Start", "start", "")]), logs);

  const one = (f) => [{ key: f, label: m.name, slot: 0, values: seriesValues(samples, [id], f) }];
  chart("cpu", { title: "CPU", subtitle: "100% = 1 CPU", unit: "pct" }).update(chartData(samples, one("cpu")));
  chart("mem", { title: "Memory", unit: "bytes" })
    .update(chartData(samples, one("mem"), memLimit ? { limit: { value: memLimit, label: `limit ${fmtBytes(memLimit)}` } } : {}));
  chart("net", { title: "Network", unit: "rate" })
    .update(chartData(samples, ioSeries(samples, [id], ["netRx", "netTx"], ["Received", "Sent"])));
  chart("disk", { title: "Disk I/O", unit: "rate" })
    .update(chartData(samples, ioSeries(samples, [id], ["blkRead", "blkWrite"], ["Read", "Written"])));
  chart("pids", { title: "Processes", subtitle: "PIDs", unit: "count" }).update(chartData(samples, one("pids")));

  renderDetails(m);
  $("#res-body").replaceChildren();
  $("#res-foot").textContent = "";
}

function renderDetails(m) {
  const info = $("#res-info");
  const x = RES.inspect;
  // Inspect data only changes on load; re-rendering would close open <details>.
  if (x && info.__inspect === x) return;
  info.__inspect = x;
  if (!x) return info.replaceChildren(el("span", { className: "muted" }, "Loading details…"));
  if (x.error) return info.replaceChildren(el("span", { className: "muted" }, x.error));
  const st = x.State ?? {}, hc = x.HostConfig ?? {}, cfg = x.Config ?? {}, ns = x.NetworkSettings ?? {};
  const started = st.StartedAt && !st.StartedAt.startsWith("0001") ? new Date(st.StartedAt) : null;
  const ports = Object.entries(ns.Ports ?? {}).flatMap(([p, binds]) =>
    binds?.length
      ? binds.map((b) => `${b.HostIp && !["0.0.0.0", "::"].includes(b.HostIp) ? b.HostIp + ":" : ""}${b.HostPort} → ${p}`)
      : [`${p} (not published)`]);
  const uniquePorts = [...new Set(ports)];
  const nets = Object.entries(ns.Networks ?? {}).map(([n, v]) => (v.IPAddress ? `${n} (${v.IPAddress})` : n));
  const mounts = (x.Mounts ?? []).map((mt) => `${mt.Type === "volume" ? mt.Name : mt.Source} → ${mt.Destination}${mt.RW ? "" : " (ro)"}`);
  const labels = cfg.Labels ?? {};
  const cpuLimit = hc.NanoCpus ? `${hc.NanoCpus / 1e9} CPUs` : hc.CpuQuota > 0 ? `${hc.CpuQuota / (hc.CpuPeriod || 100000)} CPUs` : "none";
  const rows = [
    ["State", `${st.Status ?? m.state}${st.Health ? ` · ${st.Health.Status}` : ""}${started && st.Running ? ` · since ${started.toLocaleString()}` : ""}`],
    ["Image", cfg.Image ?? m.image],
    ["ID", (x.Id ?? m.id).slice(0, 12)],
    ["Command", [x.Path, ...(x.Args ?? [])].filter(Boolean).map(quoteWord).join(" ")],
    ["Limits", `CPU ${cpuLimit} · memory ${hc.Memory ? fmtBytes(hc.Memory) : "none"}`],
    ["Restart", `${hc.RestartPolicy?.Name || "no"}${x.RestartCount ? ` · restarted ${x.RestartCount}×` : ""}`],
    ["Ports", uniquePorts.join(", ") || "—"],
    ["Networks", nets.join(", ") || "—"],
  ];
  const grid = el("dl", { className: "res-details" },
    ...rows.flatMap(([k, v]) => [el("dt", {}, k), el("dd", { className: k === "Command" || k === "ID" ? "mono" : "" }, v)]));
  const project = labels["com.docker.compose.project"];
  if (project) {
    const a = el("button", { className: "link" }, project);
    a.onclick = () => resGo({ kind: "group", project });
    grid.append(el("dt", {}, "Compose"), el("dd", {}, a, ` · service ${labels["com.docker.compose.service"] ?? "?"}`));
  }
  const more = [];
  if (mounts.length) more.push(el("details", {}, el("summary", {}, `Mounts (${mounts.length})`), el("pre", { className: "res-pre" }, mounts.join("\n"))));
  if (cfg.Env?.length) more.push(el("details", {}, el("summary", {}, `Environment (${cfg.Env.length})`), el("pre", { className: "res-pre" }, cfg.Env.join("\n"))));
  info.replaceChildren(grid, ...more);
}
