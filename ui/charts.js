// Small SVG time-series charts for the Resources page (no dependencies).
//
//   const c = new TimeChart(parent, { title, unit: "pct" | "bytes" | "rate" | "count", stacked });
//   c.update({ times, series: [{ key, label, slot, values }], t0, t1, limit, gapMs });
//
// `values` line up with `times` (null = no reading). A chart keeps its hover
// position across updates, so the crosshair stays put while data streams in.
// Colors come from --series-N tokens (style.css), assigned per entity by the
// caller and never by rank; text always uses text tokens.

const SVG_NS = "http://www.w3.org/2000/svg";
const svg = (tag, attrs = {}) => {
  const n = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
  return n;
};
const seriesColor = (slot) => (slot == null ? "var(--series-other)" : `var(--series-${slot + 1})`);

// ------------------------------------------------------------- formatting

const BIN = ["B", "KiB", "MiB", "GiB", "TiB"];

function fmtBytes(n, digits = 1) {
  if (n == null || !isFinite(n)) return "—";
  let i = 0;
  while (Math.abs(n) >= 1024 && i < BIN.length - 1) { n /= 1024; i++; }
  const s = i === 0 || n >= 100 ? Math.round(n).toString() : n.toFixed(digits).replace(/\.0+$/, "");
  return `${s} ${BIN[i]}`;
}

function fmtValue(v, unit) {
  if (v == null || !isFinite(v)) return "—";
  switch (unit) {
    case "pct": return `${v >= 100 ? Math.round(v) : v >= 10 ? v.toFixed(1) : v.toFixed(2)}%`;
    case "bytes": return fmtBytes(v);
    case "rate": return `${fmtBytes(v)}/s`;
    default: return Math.round(v).toLocaleString();
  }
}

function niceStep(raw) {
  const p = 10 ** Math.floor(Math.log10(raw));
  const f = raw / p;
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10) * p;
}

/** Clean y ticks from 0 to at least `max`. Byte units step in their own unit (0, 256 MiB, 512 MiB…). */
function yTicks(max, unit, count = 4) {
  const floor = { pct: 5, bytes: 64 * 1024 ** 2, rate: 4 * 1024, count: 4 }[unit] ?? 1;
  max = Math.max(max, floor);
  let base = 1;
  if (unit === "bytes" || unit === "rate") while (max / base >= 1024 && base < 1024 ** 4) base *= 1024;
  const step = niceStep(max / base / count) * base;
  const top = Math.ceil(max / step - 1e-9) * step;
  const ticks = [];
  for (let v = 0; v <= top + step / 2; v += step) ticks.push(v);
  return { top, ticks };
}

function tickLabel(v, unit) {
  if (unit === "pct") return `${+v.toFixed(1)}%`;
  if (unit === "bytes" || unit === "rate") return v === 0 ? "0" : fmtBytes(v, 2);
  return v >= 10000 ? `${+(v / 1000).toFixed(1)}K` : v.toLocaleString();
}

const pad2 = (n) => String(n).padStart(2, "0");
const clock = (t, secs) => {
  const d = new Date(t);
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}${secs ? ":" + pad2(d.getSeconds()) : ""}`;
};

// ------------------------------------------------------------------ chart

class TimeChart {
  constructor(parent, { title, subtitle = "", unit = "count", stacked = false, height = 150 }) {
    this.unit = unit;
    this.stacked = stacked;
    this.height = height;
    this.hoverX = null; // pointer x in plot coordinates, kept across updates
    this.root = el("figure", { className: "chart" });
    this.head = el("figcaption", { className: "chart-head" },
      el("span", { className: "chart-title" }, title),
      el("span", { className: "chart-sub" }, subtitle));
    this.now = el("span", { className: "chart-now" });
    this.head.append(this.now);
    this.legend = el("div", { className: "chart-legend" });
    this.plot = el("div", { className: "chart-plot" });
    this.tip = el("div", { className: "chart-tip", hidden: true });
    this.plot.append(this.tip);
    this.root.append(this.head, this.legend, this.plot);
    parent.append(this.root);
    this.plot.__chart = this;
    chartResize.observe(this.plot);
  }

  setSubtitle(s) { this.head.querySelector(".chart-sub").textContent = s; }

  update(data) {
    this.data = data;
    this.render();
  }

  render() {
    const d = this.data;
    if (!d) return;
    const W = Math.max(260, this.plot.clientWidth || this.root.clientWidth || 400);
    const H = this.height;
    const M = { l: 58, r: 12, t: 8, b: 22 };
    const pw = W - M.l - M.r, ph = H - M.t - M.b;
    const { times, series, t0, t1 } = d;
    const gapMs = d.gapMs ?? 10_000;

    // Stack (bottom = first series) or plain.
    const stackTop = series.map(() => new Array(times.length).fill(null));
    const stackBase = series.map(() => new Array(times.length).fill(0));
    for (let i = 0; i < times.length; i++) {
      let acc = 0;
      series.forEach((s, k) => {
        const v = s.values[i];
        if (this.stacked) {
          stackBase[k][i] = acc;
          if (v != null) acc += v;
          stackTop[k][i] = acc;
        } else {
          stackTop[k][i] = v;
        }
      });
    }
    let max = 0;
    for (let k = 0; k < series.length; k++) for (let i = 0; i < times.length; i++) {
      if (times[i] >= t0 && stackTop[k][i] != null) max = Math.max(max, stackTop[k][i]);
    }
    const limit = d.limit && d.limit.value > 0 ? d.limit : null;
    // Show a limit line only when it's in reach of the data, so it doesn't flatten everything.
    const showLimit = limit && limit.value <= max * 1.6;
    if (showLimit) max = Math.max(max, limit.value);
    const { top, ticks } = yTicks(max, this.unit);
    const x = (t) => M.l + ((t - t0) / (t1 - t0)) * pw;
    const y = (v) => M.t + ph - (v / top) * ph;

    const s = svg("svg", { width: W, height: H, viewBox: `0 0 ${W} ${H}`, class: "chart-svg", role: "img" });

    // Grid + y axis.
    for (const v of ticks) {
      s.append(svg("line", { x1: M.l, x2: W - M.r, y1: y(v), y2: y(v), class: "grid" }));
      const t = svg("text", { x: M.l - 6, y: y(v) + 3.5, class: "tick", "text-anchor": "end" });
      t.textContent = tickLabel(v, this.unit);
      s.append(t);
    }
    // x ticks at round clock times.
    const span = t1 - t0;
    const step = span <= 90_000 ? 15_000 : span <= 360_000 ? 60_000 : span <= 1_000_000 ? 180_000 : 300_000;
    for (let t = Math.ceil(t0 / step) * step; t <= t1; t += step) {
      const tx = x(t);
      if (tx < M.l + 16 || tx > W - M.r - 16) continue;
      const lab = svg("text", { x: tx, y: H - 6, class: "tick", "text-anchor": "middle" });
      lab.textContent = clock(t, step < 60_000);
      s.append(lab);
    }

    // Visible index range (+1 on each side so lines reach the edges).
    let i0 = times.findIndex((t) => t >= t0);
    if (i0 < 0) i0 = times.length;
    i0 = Math.max(0, i0 - 1);

    // Paths, split at gaps (sampler paused, container not running).
    const segments = (k) => {
      const segs = [];
      let cur = null;
      for (let i = i0; i < times.length; i++) {
        const v = series[k].values[i];
        const gap = i > i0 && times[i] - times[i - 1] > gapMs;
        if (v == null || gap) { if (cur) segs.push(cur); cur = null; }
        if (v == null) continue;
        (cur ??= []).push(i);
      }
      if (cur) segs.push(cur);
      return segs;
    };
    const clipId = `clip-${Math.random().toString(36).slice(2)}`;
    const clip = svg("clipPath", { id: clipId });
    clip.append(svg("rect", { x: M.l, y: 0, width: pw, height: H }));
    s.append(clip);
    const g = svg("g", { "clip-path": `url(#${clipId})` });
    series.forEach((ser, k) => {
      const color = seriesColor(ser.slot);
      for (const seg of segments(k)) {
        const pts = seg.map((i) => `${x(times[i]).toFixed(1)},${y(stackTop[k][i]).toFixed(1)}`);
        if (this.stacked || series.length === 1) {
          const base = seg.slice().reverse().map((i) => `${x(times[i]).toFixed(1)},${y(stackBase[k][i]).toFixed(1)}`);
          g.append(svg("polygon", { points: [...pts, ...base].join(" "), fill: color, class: this.stacked ? "area stacked" : "area" }));
        }
        if (seg.length === 1) {
          const i = seg[0];
          g.append(svg("circle", { cx: x(times[i]), cy: y(stackTop[k][i]), r: 2.5, fill: color }));
        } else {
          g.append(svg("polyline", { points: pts.join(" "), stroke: color, class: "line" }));
        }
      }
    });
    s.append(g);

    if (showLimit) {
      s.append(svg("line", { x1: M.l, x2: W - M.r, y1: y(limit.value), y2: y(limit.value), class: "limit" }));
      const lt = svg("text", { x: W - M.r - 2, y: y(limit.value) - 4, class: "tick limit-label", "text-anchor": "end" });
      lt.textContent = limit.label;
      s.append(lt);
    }

    // Hover layer.
    const cross = svg("line", { y1: M.t, y2: M.t + ph, class: "cross", visibility: "hidden" });
    const dots = svg("g", { visibility: "hidden" });
    s.append(cross, dots);
    const hit = svg("rect", { x: M.l, y: 0, width: pw, height: H, fill: "transparent", class: "hit" });
    s.append(hit);
    const showAt = (px) => {
      if (px == null || !times.length) return hide();
      const t = t0 + ((px - M.l) / pw) * (t1 - t0);
      let best = -1, bd = Infinity;
      for (let i = i0; i < times.length; i++) {
        const dd = Math.abs(times[i] - t);
        if (dd < bd) { bd = dd; best = i; }
      }
      if (best < 0 || times[best] < t0 - gapMs) return hide();
      const cx = x(times[best]);
      cross.setAttribute("x1", cx); cross.setAttribute("x2", cx);
      cross.setAttribute("visibility", "visible");
      dots.replaceChildren();
      series.forEach((ser, k) => {
        if (ser.values[best] == null) return;
        dots.append(svg("circle", { cx, cy: y(stackTop[k][best]), r: 4, fill: seriesColor(ser.slot), class: "dot" }));
      });
      dots.setAttribute("visibility", "visible");
      const rows = series
        .map((ser, k) => ({ ser, v: ser.values[best], k }))
        .filter((r) => r.v != null);
      // Top of the stack first, so the list reads like the chart.
      if (this.stacked) rows.reverse();
      const tip = [el("div", { className: "tip-time" }, clock(times[best], true))];
      for (const r of rows) {
        tip.push(el("div", { className: "tip-row" },
          el("i", { className: "swatch", style: `background:${seriesColor(r.ser.slot)}` }),
          el("span", {}, r.ser.label),
          el("b", {}, fmtValue(r.v, this.unit))));
      }
      if (this.stacked && rows.length > 1) {
        tip.push(el("div", { className: "tip-row total" }, el("i"), el("span", {}, "Total"),
          el("b", {}, fmtValue(rows.reduce((a, r) => a + r.v, 0), this.unit))));
      }
      this.tip.replaceChildren(...tip);
      this.tip.hidden = false;
      const tw = this.tip.offsetWidth;
      this.tip.style.left = `${cx + 12 + tw > W ? cx - tw - 12 : cx + 12}px`;
      this.tip.style.top = `${M.t}px`;
    };
    const hide = () => {
      cross.setAttribute("visibility", "hidden");
      dots.setAttribute("visibility", "hidden");
      this.tip.hidden = true;
    };
    hit.addEventListener("mousemove", (e) => {
      this.hoverX = e.clientX - s.getBoundingClientRect().left;
      showAt(this.hoverX);
    });
    hit.addEventListener("mouseleave", () => { this.hoverX = null; hide(); });

    this.plot.querySelector("svg")?.remove();
    this.plot.prepend(s);
    if (this.hoverX != null) showAt(this.hoverX);

    // Legend (2+ series) and the latest value(s) in the header.
    this.legend.hidden = series.length < 2;
    this.legend.replaceChildren(...series.map((ser) =>
      el("span", { className: "legend-item", title: ser.title ?? ser.label },
        el("i", { className: "swatch", style: `background:${seriesColor(ser.slot)}` }), ser.label)));
    const last = times.length - 1;
    const latest = series.map((ser) => ser.values[last]).filter((v) => v != null);
    this.now.textContent = !latest.length ? "" : this.stacked || series.length === 1
      ? fmtValue(latest.reduce((a, b) => a + b, 0), this.unit) + (limit ? ` / ${fmtValue(limit.value, this.unit)}` : "")
      : series.map((ser) => `${ser.short ?? ser.label} ${fmtValue(ser.values[last], this.unit)}`).join(" · ");
  }
}

/** A tiny trend line (de-emphasis ink) for table cells. */
function sparkline(values, { width = 72, height = 18, max } = {}) {
  const s = svg("svg", { width, height, class: "spark", "aria-hidden": "true" });
  const vs = values.filter((v) => v != null);
  if (vs.length < 2) return s;
  const top = Math.max(max ?? 0, ...vs, 1e-9);
  const n = values.length;
  const pts = [];
  values.forEach((v, i) => {
    if (v == null) return;
    pts.push(`${((i / (n - 1)) * (width - 2) + 1).toFixed(1)},${(height - 1 - (v / top) * (height - 3)).toFixed(1)}`);
  });
  s.append(svg("polyline", { points: pts.join(" ") }));
  return s;
}

// Re-render a chart when its width changes.
const chartResize = new ResizeObserver((entries) => {
  for (const e of entries) e.target.__chart?.render();
});
