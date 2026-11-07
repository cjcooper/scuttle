// About window: the app, its own license, and every third-party license
// (THIRD-PARTY-NOTICES.txt, compiled into the app), one collapsible section
// per license with the software it covers.
const { invoke } = window.__TAURI__.core;
const openUrl = (url) => window.__TAURI__.opener.openUrl(url);
const $ = (s) => document.querySelector(s);
const el = (tag, props = {}, ...children) => {
  const n = Object.assign(document.createElement(tag), props);
  for (const c of children) n.append(c instanceof Node ? c : document.createTextNode(c ?? ""));
  return n;
};

const RULE = "=".repeat(80);
const BUILT_WITH = [
  ["Tauri", "https://tauri.app"],
  ["xterm.js", "https://xtermjs.org"],
  ["portable-pty", "https://github.com/wezterm/wezterm/tree/main/pty"],
  ["Tokio", "https://tokio.rs"],
];

/** Split the notices file into { title, usedBy[], text } blocks. */
function parseNotices(all) {
  const blocks = all.split(RULE + "\n");
  blocks.shift(); // intro
  return blocks.map((block) => {
    const nl = block.indexOf("\n");
    const title = block.slice(0, nl).trim();
    let rest = block.slice(nl + 1).replace(/^\n+/, "");
    let usedBy = [];
    if (rest.startsWith("Used by:\n")) {
      rest = rest.slice("Used by:\n".length);
      const end = rest.indexOf("\n\n") === -1 ? rest.length : rest.indexOf("\n\n");
      usedBy = rest.slice(0, end).split("\n").map((l) => l.trim()).filter(Boolean);
      rest = rest.slice(end);
    }
    return { title, usedBy, text: rest.replace(/^\n+|\n+$/g, "") };
  });
}

/** Short license kind for the filter chips. */
function kind(n) {
  if (n.title.startsWith("Vendored JavaScript")) return "Vendored";
  return n.title.replace(/ License$/, "").replace(/^MIT No Attribution$/, "MIT-0");
}

function showTab(name) {
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.tab === name));
  document.querySelectorAll(".about-pane").forEach((p) => p.classList.toggle("active", p.id === `pane-${name}`));
}
document.querySelectorAll(".tab").forEach((t) => (t.onclick = () => showTab(t.dataset.tab)));

const link = (name, url) => {
  const a = el("a", { href: url }, name);
  a.onclick = (e) => { e.preventDefault(); openUrl(url); };
  return a;
};

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey) invoke("close_about");
});

(async () => {
  const info = await invoke("about_info");
  const notices = parseNotices(info.notices);
  const crates = notices.filter((n) => kind(n) !== "Vendored").reduce((sum, n) => sum + n.usedBy.length, 0);
  const vendored = notices.length - notices.filter((n) => kind(n) !== "Vendored").length;

  document.title = `About ${info.name}`;
  $("#about-name").textContent = info.name;
  $("#about-version").textContent = `Version ${info.version}`;
  $("#about-description").textContent = info.description;
  $("#about-maker").textContent = `Made by ${info.maker}. ${info.copyright}`;
  if (info.repository) {
    $("#about-source").hidden = $("#about-source-sep").hidden = false;
    $("#about-source").onclick = (e) => { e.preventDefault(); openUrl(info.repository); };
  }
  $("#about-licenses").onclick = (e) => { e.preventDefault(); showTab("third-party"); };
  const built = $("#about-built");
  built.append("Built with ");
  BUILT_WITH.forEach(([name, url], i) => {
    if (i) built.append(i === BUILT_WITH.length - 1 ? " and " : ", ");
    built.append(link(name, url));
  });
  built.append(`, plus ${crates} open-source Rust crates.`);
  $("#about-license").textContent = info.license.trim();

  $("#third-party-tab").textContent = `Third-party licenses (${crates + vendored})`;
  $("#tp-intro").textContent =
    `${info.name} includes ${crates} Rust crates and ${vendored} vendored JavaScript ${vendored === 1 ? "library" : "libraries"}. ` +
    "Each license is shown with the software it covers.";

  const counts = {};
  for (const n of notices) counts[kind(n)] = (counts[kind(n)] || 0) + Math.max(n.usedBy.length, 1);
  let chip = null;
  const chips = $("#tp-chips");
  const renderChips = () => {
    chips.replaceChildren(
      ...[[null, `All ${crates + vendored}`], ...Object.entries(counts).sort((a, b) => b[1] - a[1]).map(([k, c]) => [k, `${k} ${c}`])]
        .map(([k, label]) => {
          const b = el("button", { className: "chip-button" + (chip === k ? " on" : "") }, label);
          b.onclick = () => { chip = chip === k ? null : k; renderChips(); renderList(); };
          return b;
        })
    );
  };
  const renderList = () => {
    const f = $("#tp-filter").value.trim().toLowerCase();
    const shown = notices.filter((n) =>
      (!chip || kind(n) === chip) &&
      (!f || n.title.toLowerCase().includes(f) || n.usedBy.some((c) => c.toLowerCase().includes(f))));
    $("#tp-list").replaceChildren(
      ...(shown.length
        ? shown.map((n) => {
            const names = n.usedBy.map((c) => c.split(" ")[0]);
            const more = names.length > 3 ? ` and ${names.length - 3} more` : "";
            const summary = n.usedBy.length ? `${n.title} — ${names.slice(0, 3).join(", ")}${more}` : n.title;
            return el("details", { className: "about-notice" },
              el("summary", {}, summary),
              ...(n.usedBy.length ? [el("ul", { className: "about-crates mono" }, ...n.usedBy.map((c) => el("li", {}, c)))] : []),
              el("pre", { className: "about-text" }, n.text));
          })
        : [el("p", { className: "muted" }, "Nothing matches.")])
    );
  };
  $("#tp-filter").oninput = renderList;
  renderChips();
  renderList();
})();
