// One dropdown style for every <select>, with optional option icons.
//
// Each <select> stays in the page as the source of truth (code keeps reading
// .value, replacing options and listening for "change"); it is hidden and a
// button + menu drawn next to it mirrors it. An option shows an icon when it
// has data-icon (and data-icon-dark for a dark-mode variant).
(() => {
  const dark = matchMedia("(prefers-color-scheme: dark)");
  const valueDesc = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value");
  const indexDesc = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "selectedIndex");
  let open = null; // the dropdown whose menu is showing

  function iconFor(opt) {
    const src = (dark.matches && opt.dataset.iconDark) || opt.dataset.icon;
    if (!src) return null;
    const img = document.createElement("img");
    img.className = "dd-icon";
    img.src = src;
    img.alt = "";
    return img;
  }

  function enhance(sel) {
    if (sel.dataset.dd) return;
    sel.dataset.dd = "1";
    sel.classList.add("dd-native");
    sel.tabIndex = -1;

    const wrap = document.createElement("span");
    wrap.className = "dd";
    const button = document.createElement("button");
    button.type = "button";
    button.className = "dd-button";
    button.setAttribute("aria-haspopup", "listbox");
    const face = document.createElement("span");
    face.className = "dd-face";
    const chevron = document.createElement("span");
    chevron.className = "dd-chevron";
    chevron.setAttribute("aria-hidden", "true");
    button.append(face, chevron);
    const menu = document.createElement("div");
    menu.className = "dd-menu";
    menu.setAttribute("role", "listbox");
    menu.hidden = true;
    sel.after(wrap);
    wrap.append(sel, button, menu);

    const dd = { sel, button, menu, active: -1 };

    const render = () => {
      const opt = sel.options[sel.selectedIndex];
      face.replaceChildren(...[opt && iconFor(opt), document.createTextNode(opt?.textContent ?? "")].filter(Boolean));
      button.disabled = sel.disabled;
      wrap.hidden = sel.hidden;
      button.title = sel.title || opt?.textContent || "";
      if (!menu.hidden) fill();
    };
    const fill = () => {
      menu.replaceChildren(
        ...[...sel.options].map((o, i) => {
          const item = document.createElement("div");
          item.className = "dd-item";
          item.setAttribute("role", "option");
          item.setAttribute("aria-selected", String(i === sel.selectedIndex));
          if (o.disabled) item.setAttribute("aria-disabled", "true");
          if (i === dd.active) item.classList.add("active");
          item.append(...[iconFor(o), document.createTextNode(o.textContent)].filter(Boolean));
          item.onmousemove = () => setActive(i);
          item.onmousedown = (e) => e.preventDefault(); // keep focus on the button
          item.onclick = () => pick(i);
          return item;
        })
      );
    };
    const setActive = (i) => {
      if (i === dd.active) return;
      dd.active = i;
      menu.querySelectorAll(".dd-item").forEach((n, j) => n.classList.toggle("active", j === i));
      menu.children[i]?.scrollIntoView({ block: "nearest" });
    };
    const pick = (i) => {
      const o = sel.options[i];
      if (!o || o.disabled) return;
      close();
      if (i !== sel.selectedIndex) {
        indexDesc.set.call(sel, i);
        render();
        sel.dispatchEvent(new Event("input", { bubbles: true }));
        sel.dispatchEvent(new Event("change", { bubbles: true }));
      }
      button.focus();
    };
    const place = () => {
      const r = button.getBoundingClientRect();
      menu.style.minWidth = `${r.width}px`;
      menu.style.left = `${r.left}px`;
      const below = innerHeight - r.bottom - 8, above = r.top - 8;
      const flip = below < Math.min(menu.scrollHeight, 240) && above > below;
      menu.style.maxHeight = `${Math.max(120, flip ? above : below)}px`;
      menu.style.top = flip ? "" : `${r.bottom + 4}px`;
      menu.style.bottom = flip ? `${innerHeight - r.top + 4}px` : "";
      const overflow = menu.getBoundingClientRect().right - (innerWidth - 8);
      if (overflow > 0) menu.style.left = `${Math.max(8, r.left - overflow)}px`;
    };
    const show = () => {
      if (sel.disabled) return;
      close();
      open = dd;
      dd.active = sel.selectedIndex;
      menu.hidden = false;
      wrap.classList.add("open");
      button.setAttribute("aria-expanded", "true");
      fill();
      place();
      menu.children[dd.active]?.scrollIntoView({ block: "nearest" });
    };
    const step = (from, dir) => {
      for (let i = from + dir; i >= 0 && i < sel.options.length; i += dir) if (!sel.options[i].disabled) return i;
      return from;
    };
    dd.close = () => {
      menu.hidden = true;
      wrap.classList.remove("open");
      button.setAttribute("aria-expanded", "false");
    };

    button.addEventListener("click", () => (menu.hidden ? show() : close()));
    button.addEventListener("keydown", (e) => {
      const shown = !menu.hidden;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const dir = e.key === "ArrowDown" ? 1 : -1;
        if (shown) setActive(step(dd.active, dir));
        else if (e.altKey) show();
        else pick(step(sel.selectedIndex, dir));
      } else if ((e.key === "Enter" || e.key === " ") && shown) {
        e.preventDefault();
        pick(dd.active);
      } else if (e.key === "Escape" && shown) {
        e.preventDefault();
        e.stopPropagation(); // don't also close an enclosing dialog
        close();
      } else if (e.key === "Tab" && shown) {
        close();
      } else if (e.key.length === 1 && !e.metaKey && !e.ctrlKey) {
        // Type-to-select: jump to the next option starting with that letter.
        const k = e.key.toLowerCase(), n = sel.options.length, from = shown ? dd.active : sel.selectedIndex;
        for (let j = 1; j <= n; j++) {
          const i = (from + j) % n, o = sel.options[i];
          if (!o.disabled && o.textContent.trim().toLowerCase().startsWith(k)) {
            shown ? setActive(i) : pick(i);
            break;
          }
        }
      }
    });

    // Keep the face current when code changes the select.
    Object.defineProperty(sel, "value", {
      configurable: true,
      get() { return valueDesc.get.call(this); },
      set(v) { valueDesc.set.call(this, v); render(); },
    });
    Object.defineProperty(sel, "selectedIndex", {
      configurable: true,
      get() { return indexDesc.get.call(this); },
      set(v) { indexDesc.set.call(this, v); render(); },
    });
    new MutationObserver(render).observe(sel, {
      childList: true, subtree: true, characterData: true,
      attributes: true, attributeFilter: ["disabled", "hidden", "selected", "title", "data-icon", "data-icon-dark"],
    });
    sel.addEventListener("change", render);
    // A click on the enclosing <label> focuses the hidden select; hand it on.
    sel.addEventListener("focus", () => button.focus());
    dark.addEventListener("change", render);
    render();
  }

  function close() {
    open?.close();
    open = null;
  }

  document.addEventListener("mousedown", (e) => {
    if (open && !open.button.parentElement.contains(e.target)) close();
  });
  // The menu is position: fixed, so anything that moves the button closes it.
  addEventListener("resize", close);
  document.addEventListener("scroll", (e) => { if (open && !open.menu.contains(e.target)) close(); }, true);

  const scan = (root) => {
    if (root instanceof HTMLSelectElement) enhance(root);
    else root.querySelectorAll?.("select").forEach(enhance);
  };
  scan(document);
  new MutationObserver((records) => {
    for (const r of records) r.addedNodes.forEach(scan);
  }).observe(document.documentElement, { childList: true, subtree: true });
})();
