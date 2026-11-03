"use strict";

// The settings window, for the desktop app and `anya --web`. Pages behind a
// sidebar; every change saves at once: a control posts the whole
// config to /api/settings and the answer is the new state. `state` is the
// last state the server sent, and the only copy of the config.

let state = null;
// Whether each engine's settings are enough to open a session, kept live
// while the engine form is typed into.
let ready = {};
// The engine form's values per engine, ahead of what is saved.
let typed = {};
let hotkeyWarning = null;
let page = null;
// Updates the current page from the status poll.
let live = null;
let lastStatus = null;
let saveChain = Promise.resolve();

const PAGES = ["activity", "route", "dictation", "continuous", "engine", "plugins", "overlay", "apps", "general"];
// The sidebar's groups, in PAGES order. A group without a label sits alone;
// the last one is pinned to the foot of the sidebar.
const NAV_GROUPS = [
  { pages: ["activity", "route"] },
  { label: "gui.nav.group_transcribe", pages: ["dictation", "continuous"] },
  { label: "gui.nav.group_recognize", pages: ["engine", "plugins"] },
  { label: "gui.nav.group_output", pages: ["overlay", "apps"] },
  { pages: ["general"], foot: true },
];
const NAV = {
  activity: "gui.nav.activity",
  route: "gui.nav.route",
  dictation: "gui.nav.dictation",
  continuous: "gui.nav.continuous",
  engine: "gui.nav.engine",
  plugins: "gui.nav.plugins",
  overlay: "gui.nav.overlay",
  apps: "gui.nav.apps",
  general: "gui.nav.general",
};
const SUBTITLE = {
  activity: "gui.activity.subtitle",
  route: "gui.route.subtitle",
  dictation: "gui.dictation.subtitle",
  continuous: "gui.continuous.subtitle",
  engine: "gui.engine.subtitle",
  plugins: "gui.plugins.subtitle",
  overlay: "gui.overlay.subtitle",
  apps: "gui.apps.subtitle",
  general: "gui.general.subtitle",
};

const clone = (v) => JSON.parse(JSON.stringify(v));
const cfg = () => state.config;
const isApp = () => state.shell === "app";
const platform = () => state.info.platform;

function getPath(obj, path) {
  return path.split(".").reduce((o, k) => o?.[k], obj);
}

function setPath(obj, path, value) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((o, k) => o[k], obj)[last] = value;
}

// ---- saving ----

function notify(text, kind = "") {
  if (!text) dismissToast();
  else toast(text, kind || "ok");
}

function errorSlot(path) {
  return el("p", { class: "field-error", "data-error": path, role: "alert", hidden: true });
}

function showError(path, text) {
  for (const node of document.querySelectorAll(`[data-error="${path}"]`)) {
    node.textContent = text;
    node.hidden = !text;
  }
}

// Puts each refusal under the field it is about; true when every one found
// its field on this page.
function showInvalid(list) {
  const byField = new Map();
  for (const { fields, message } of list) {
    for (const f of fields) byField.set(f, [...(byField.get(f) || []), message]);
  }
  let placed = byField.size > 0;
  for (const [field, messages] of byField) {
    if (!document.querySelector(`[data-error="${field}"]`)) placed = false;
    showError(field, messages.join(t("gui.list_separator")));
  }
  return placed;
}

// Posts the config with `mutate` applied. Saves run one after another, each
// on the config the previous one produced.
function commit(mutate, { field, revert } = {}) {
  const run = async () => {
    if (toastIsError()) dismissToast();
    const next = clone(cfg());
    mutate(next);
    if (field) showError(field, "");
    try {
      afterSave(await api("/api/settings", next));
      return true;
    } catch (err) {
      notify(err.message, "error");
      // A refusal with a place on this page stays there, with the rejected
      // value; anything else puts the controls back as saved.
      const placed = err.data?.invalid && showInvalid(err.data.invalid);
      if (revert) revert();
      else if (!placed) renderPage();
      return false;
    }
  };
  const result = saveChain.then(run);
  saveChain = result;
  return result;
}

const FLASH = "anya.warning";

function afterSave(res) {
  applyState(res.state, { render: false });
  hotkeyWarning = res.warning ?? null;
  // A new interface language: load the page again in it.
  if (state.locale !== LOCALE) {
    try { sessionStorage.setItem(FLASH, hotkeyWarning ?? ""); } catch {}
    location.reload();
    return;
  }
  drawHotkeyBanners();
  toast(t("gui.saved"));
  if (page === "engine") engineViews?.paint();
  else drawEngineLine();
}

function applyState(fresh, { render = true } = {}) {
  state = fresh;
  ready = Object.fromEntries(state.engines.map((e) => [e.id, e.ready]));
  typed = {};
  document.body.classList.toggle("mac-app", isApp() && platform() === "macos");
  if (render && page) renderPage();
}

// ---- widgets ----

function row(label, help, control, { error, top, stack } = {}) {
  const text = el("div", { class: "label" }, el("span", {}, label));
  const small = help == null ? null : el("small", {}, help);
  if (small) text.append(small);
  if (error) text.append(errorSlot(error));
  const node = el("div", { class: stack ? "row stack" : top ? "row top" : "row" }, text, control);
  node.help = small;
  return node;
}

// A titled box of rows, as System Settings groups them. `action` sits at the
// right of the title, `note` under the box.
function group(title, rows, { action, note } = {}) {
  return el("section", { class: "section" },
    title ? el("h2", {}, title, el("span", { class: "spacer" }), action ?? null) : null,
    el("div", { class: "group" }, ...rows),
    note ? el("p", { class: "foot-note" }, note) : null);
}

// A long value (a path, an address): one line cut with an ellipsis, the whole
// of it in the tooltip, and a button to copy it.
function pathLine(text) {
  return el("div", { class: "paths" }, el("code", { title: text }, text), copyButton(text));
}

function ctl(...children) {
  return el("div", { class: "ctl" }, ...children);
}

function bindToggle(path, aria) {
  const input = el("input", { type: "checkbox", class: "switch", "aria-label": aria });
  input.checked = !!getPath(cfg(), path);
  input.addEventListener("change", () => commit((c) => setPath(c, path, input.checked), {
    field: path,
    revert: () => { input.checked = !!getPath(cfg(), path); },
  }));
  return input;
}

function selectBox(options, value, onchange, aria) {
  const select = el("select", { "aria-label": aria, onchange: () => onchange(select.value) },
    ...options.map((o) => el("option", { value: o.value }, o.label)));
  select.value = value;
  return select;
}

function bindSelect(path, options, aria) {
  const select = selectBox(options, getPath(cfg(), path) ?? "", (value) => commit((c) => setPath(c, path, value), {
    field: path,
    revert: () => { select.value = getPath(cfg(), path) ?? ""; },
  }), aria);
  return select;
}

// A text field that saves when it loses focus or Enter is pressed.
function bindText(path, { placeholder, aria } = {}) {
  const input = el("input", { type: "text", spellcheck: "false", placeholder, "aria-label": aria });
  input.value = getPath(cfg(), path) ?? "";
  input.addEventListener("change", () => {
    input.classList.remove("invalid");
    commit((c) => setPath(c, path, input.value), { field: path });
  });
  return input;
}

function segmented(options, value, onchange, aria) {
  const bg = el("span", { class: "pill-bg still" });
  const buttons = options.map(([v, label]) => el("button", {
    type: "button",
    role: "radio",
    onclick: () => choose(v),
  }, label));
  const box = el("div", { class: "segmented", role: "radiogroup", "aria-label": aria }, bg, ...buttons);
  let current = value;
  const place = () => {
    buttons.forEach((b, i) => b.setAttribute("aria-checked", String(options[i][0] === current)));
    const b = buttons[options.findIndex(([v]) => v === current)];
    if (!b || !b.offsetWidth) return;
    bg.style.width = `${b.offsetWidth}px`;
    bg.style.transform = `translateX(${b.offsetLeft}px)`;
    // The first placement is where it starts, not a move.
    requestAnimationFrame(() => bg.classList.remove("still"));
  };
  function choose(v) {
    if (v === current) return;
    current = v;
    place();
    onchange(v);
  }
  // Puts the control back on `v` without calling `onchange`.
  box.set = (v) => { current = v; place(); };
  new ResizeObserver(place).observe(box);
  place();
  return box;
}

// A range with its value beside it; `onchange` runs when the thumb is let go.
function slider({ min, max, step, value, format, onchange, aria }) {
  const out = el("output", {}, format(value));
  const input = el("input", { type: "range", min, max, step, "aria-label": aria });
  input.value = value;
  const fill = () => input.style.setProperty("--fill", `${((input.value - min) / (max - min)) * 100}%`);
  fill();
  input.addEventListener("input", () => { out.textContent = format(Number(input.value)); fill(); });
  // `back` puts the thumb on `v` when a save was refused.
  const back = (v) => { input.value = v; out.textContent = format(v); fill(); };
  input.addEventListener("change", () => onchange(Number(input.value), back));
  return el("div", { class: "slider" }, input, out);
}

function dotLine(kind, text) {
  return el("span", { class: "state" }, el("i", { class: `dot ${kind}` }), text);
}

// ---- the page frame ----

function pageHead(name) {
  return el("header", { class: "page-head" }, el("h1", {}, t(NAV[name])), el("p", {}, t(SUBTITLE[name])));
}

function renderPage() {
  endRecording();
  live = null;
  hotkeyViews.clear();
  const builders = {
    activity: buildActivity, route: buildRoute, dictation: buildDictation, continuous: buildContinuous,
    engine: buildEngine, plugins: buildPlugins, overlay: buildOverlay, apps: buildApps, general: buildGeneral,
  };
  const root = $("page");
  root.replaceChildren(pageHead(page), ...builders[page]());
  for (const button of $("nav").querySelectorAll("button")) {
    setAttr(button, "aria-current", button.dataset.page === page ? "page" : null);
  }
  setText($("barTitle"), t(NAV[page]));
  placeNav();
  drawHotkeyBanners();
  if (lastStatus && live) live(lastStatus);
}

// Where each page was scrolled to, so coming back finds it as it was.
const scrolls = {};

function showPage(name) {
  if (!PAGES.includes(name)) name = PAGES[0];
  if (name === page) return;
  if (page) scrolls[page] = $("pane").scrollTop;
  page = name;
  try { localStorage.setItem("anya.page", name); } catch {}
  if (location.hash !== `#${name}`) history.replaceState(null, "", `#${name}`);
  if (!state) return;
  renderPage();
  $("pane").scrollTop = scrolls[name] ?? 0;
  const root = $("page");
  root.classList.remove("page-in");
  void root.offsetWidth;
  root.classList.add("page-in");
}

const ICONS = {
  activity: ["M3 12h4l3-7 4 14 3-7h4"],
  route: ["M4 7a2 2 0 1 0 4 0a2 2 0 1 0 -4 0", "M16 17a2 2 0 1 0 4 0a2 2 0 1 0 -4 0", "M8 7h7a3 3 0 0 1 3 3v5"],
  dictation: ["M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z", "M5 11a7 7 0 0 0 14 0", "M12 18v3"],
  continuous: ["M4 10v4", "M8 6v12", "M12 3v18", "M16 7v10", "M20 10v4"],
  engine: ["M8 8h8v8H8z", "M10 3v3M14 3v3M10 18v3M14 18v3M3 10h3M3 14h3M18 10h3M18 14h3"],
  plugins: ["M12 3l8 4.5v9L12 21l-8-4.5v-9z", "M4 7.5l8 4.5 8-4.5", "M12 12v9"],
  overlay: ["M3 5h18v11H3z", "M8 20h8", "M12 16v4"],
  apps: ["M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1", "M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"],
  general: ["M4 7h9M17 7h3M4 17h3M11 17h9", "M15 5v4M9 15v4"],
};

// A thin line glyph in the text colour.
function navIcon(name) {
  const svg = new DOMParser().parseFromString(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">${ICONS[name].map((d) => `<path d="${d}"/>`).join("")}</svg>`,
    "image/svg+xml").documentElement;
  return el("span", { class: "icon", "aria-hidden": "true" }, document.importNode(svg, true));
}

function drawNav() {
  const pill = el("span", { class: "nav-pill still", "aria-hidden": "true" });
  const go = (name) => {
    showPage(name);
    $("nav").querySelector(`[data-page="${name}"]`)?.focus({ preventScroll: true });
  };
  const groups = NAV_GROUPS.map((g) => el("div", { class: g.foot ? "nav-group foot" : "nav-group" },
    ...(g.label ? [el("div", { class: "nav-label" }, t(g.label))] : []),
    ...g.pages.map((name) => el("button", {
      type: "button",
      "data-page": name,
      onclick: () => go(name),
    }, navIcon(name), el("span", { class: "nav-text" }, t(NAV[name]))))));
  $("nav").replaceChildren(pill, ...groups);
  // Up and down move the selection while the sidebar has focus.
  $("nav").onkeydown = (ev) => {
    const step = { ArrowUp: -1, ArrowDown: 1 }[ev.key];
    if (!step) return;
    ev.preventDefault();
    const next = PAGES[PAGES.indexOf(page) + step];
    if (next) go(next);
  };
  new ResizeObserver(placeNav).observe($("nav"));
  drawSidebarToggle();
}

// The sidebar folds to its icons; the choice is kept across windows.
const SIDEBAR_ICON = ["M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z", "M9 5v14"];

function sidebarFolded() {
  return document.body.classList.contains("folded");
}

function drawSidebarToggle() {
  let folded = false;
  try { folded = localStorage.getItem("anya.sidebar") === "folded"; } catch {}
  $("sidebarToggle").replaceChildren(glyph(SIDEBAR_ICON));
  $("sidebarToggle").onclick = () => {
    setFolded(!sidebarFolded());
    try { localStorage.setItem("anya.sidebar", sidebarFolded() ? "folded" : "open"); } catch {}
  };
  setFolded(folded);
}

function setFolded(folded) {
  toggleClass(document.body, "folded", folded);
  const label = t(folded ? "gui.sidebar.expand" : "gui.sidebar.collapse");
  setAttr($("sidebarToggle"), "title", label);
  setAttr($("sidebarToggle"), "aria-label", label);
  setAttr($("sidebarToggle"), "aria-expanded", String(!folded));
  sidebarTitles();
}

// Folded, a row's name and the engine's state show as tooltips.
function sidebarTitles() {
  const folded = sidebarFolded();
  for (const button of $("nav").querySelectorAll("button")) {
    setAttr(button, "title", folded ? t(NAV[button.dataset.page]) : null);
  }
  setAttr($("engineLine"), "title", folded ? $("engineLine").textContent : null);
}

// The one selection pill slides to the chosen row.
function placeNav() {
  const pill = $("nav").querySelector(".nav-pill");
  const row = $("nav").querySelector(`[data-page="${page}"]`);
  if (!pill || !row || !row.offsetHeight) return;
  pill.style.height = `${row.offsetHeight}px`;
  pill.style.transform = `translateY(${row.offsetTop}px)`;
  // Its first placement is where it starts, not a move.
  requestAnimationFrame(() => pill.classList.remove("still"));
}

// The engine in use, at the foot of the sidebar: whether it can start, or
// the first thing it lacks.
function drawEngineLine() {
  const id = cfg().recognition.dictation;
  const engine = state.engines.find((e) => e.id === id);
  const title = engine?.title ?? id;
  let kind = "ok";
  let text = tf("gui.sidebar.engine_ready", { engine: title });
  if (!(ready[id] ?? engine?.ready)) {
    kind = "warn";
    const values = typed[id] ?? cfg().recognition.engines[id] ?? {};
    const lacking = engine?.fields.find((f) => f.required && !String(values[f.key] ?? "").trim());
    text = lacking
      ? tf("gui.sidebar.engine_missing", { engine: title, field: lacking.label })
      : tf("gui.sidebar.engine_unconfigured", { engine: title });
  }
  const line = $("engineLine");
  if (line.dataset.state === kind && line.lastChild?.textContent === text) return;
  line.dataset.state = kind;
  line.replaceChildren(el("i", { class: `dot ${kind}` }), el("span", { class: "nav-text" }, text));
  sidebarTitles();
}

// ---- hotkeys ----
//
// A hotkey field is chips with a "record" chip. In the desktop app the OS
// records the combination, which sees keys a webview cannot (Fn); in a
// browser, or when the OS recorder is unavailable, the page's own key events
// do, while the global hotkeys are paused.

const MOD_ORDER = ["Ctrl", "Alt", "Shift", "Super", "Fn"];
const MOD_CODES = {
  ControlLeft: "Ctrl", ControlRight: "Ctrl", AltLeft: "Alt", AltRight: "Alt",
  ShiftLeft: "Shift", ShiftRight: "Shift", MetaLeft: "Super", MetaRight: "Super",
  OSLeft: "Super", OSRight: "Super", Fn: "Fn",
};
// KeyboardEvent.code → the name Anya's hotkey parser reads.
const KEY_CODES = {
  Tab: "Tab", CapsLock: "CapsLock", Enter: "Enter", NumpadEnter: "Enter", Backspace: "Backspace",
  Space: "Space", Escape: "Escape", ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left",
  ArrowRight: "Right", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown",
  Insert: "Insert", Delete: "Delete", ContextMenu: "Menu", Backquote: "`", Minus: "-",
  Equal: "=", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Semicolon: ";",
  Quote: "'", Comma: ",", Period: ".", Slash: "/",
};

let rec = null; // { path, mode: "os" | "keys", mods: Set, ready, keepalive }
// path → redraws that field; emptied with every page.
const hotkeyViews = new Map();

function keyName(code) {
  if (KEY_CODES[code]) return KEY_CODES[code];
  let m = /^Key([A-Z])$/.exec(code) || /^Digit(\d)$/.exec(code);
  if (m) return m[1];
  if ((m = /^Numpad(\d)$/.exec(code))) return `Num${m[1]}`;
  if ((m = /^F(\d{1,2})$/.exec(code)) && +m[1] >= 1 && +m[1] <= 24) return code;
  return null;
}

function modName(ev) {
  return MOD_CODES[ev.code] || (ev.key === "Fn" ? "Fn" : null);
}

function redrawHotkeys() {
  for (const draw of hotkeyViews.values()) draw();
}

// The chips of the hotkey list at `path`.
function hotkeyField(path) {
  const chips = el("div", { class: "chips" });
  const draw = () => {
    const list = splitHotkeys(getPath(cfg(), path));
    const items = list.map((combo, i) => el("span", { class: "chip" },
      comboLabel(combo, platform()),
      el("button", {
        type: "button",
        title: tf("gui.hotkey.remove", { combo }),
        "aria-label": tf("gui.hotkey.remove", { combo }),
        onclick: async () => {
          await commit((c) => setPath(c, path, list.filter((_, j) => j !== i).join(", ")), { field: path });
          redrawHotkeys();
        },
      }, "×")));
    if (!list.length) items.push(el("span", { class: "muted" }, t("gui.hotkey.unset")));
    items.push(recordChip(path));
    chips.replaceChildren(...items);
  };
  hotkeyViews.set(path, draw);
  draw();
  return el("div", { class: "ctl col" }, chips);
}

function roleOf(path) {
  return path === "continuous.hotkey" ? "continuous" : "dictation";
}

function recordChip(path) {
  if (rec?.path === path) {
    const held = MOD_ORDER.filter((m) => rec.mods.has(m));
    const text = held.length
      ? comboLabel(held.join("+"), platform())
      : !rec.ready ? t("gui.hotkey.pausing") : t("gui.hotkey.recording");
    return el("button", { type: "button", class: "chip rec on", onclick: endRecording }, text);
  }
  return el("button", { type: "button", class: "chip rec", onclick: () => startRecording(path) }, t("gui.hotkey.record"));
}

async function startRecording(path) {
  endRecording();
  showError(path, "");
  const r = { path, mode: isApp() ? "os" : "keys", mods: new Set(), ready: false, keepalive: 0 };
  rec = r;
  if (r.mode === "os") {
    r.ready = true;
    redrawHotkeys();
    recordWithOs(r);
    return;
  }
  redrawHotkeys();
  await pauseHotkeys(r);
}

async function recordWithOs(r) {
  let value;
  try {
    ({ value } = await api("/api/hotkey/record", {}));
  } catch (err) {
    if (rec !== r) return;
    if (err.status === 409) {
      endRecording();
      showError(r.path, err.message);
      return;
    }
    // No OS recorder here (a permission, a backend without one): record
    // the keys this page sees instead.
    r.mode = "keys";
    r.ready = false;
    redrawHotkeys();
    await pauseHotkeys(r);
    return;
  }
  if (rec !== r) return;
  endRecording();
  if (value) acceptCombo(r.path, value);
}

// Keeps the global hotkeys paused while this page records keys, so the old
// hotkey does not start a session.
async function pauseHotkeys(r) {
  try {
    await api("/api/hotkey/suspend", { active: true });
  } catch (err) {
    if (rec === r) endRecording();
    showError(r.path, err.message);
    return;
  }
  if (rec !== r) return;
  r.ready = true;
  r.keepalive = setInterval(() => api("/api/hotkey/suspend", { active: true }).catch(() => {}), 5000);
  redrawHotkeys();
}

function endRecording() {
  if (!rec) return;
  const r = rec;
  rec = null;
  clearInterval(r.keepalive);
  if (r.mode === "keys") api("/api/hotkey/suspend", { active: false }).catch(() => {});
  redrawHotkeys();
}

// The server checks the combination's form; what is taken by the other
// hotkey is decided here, from the config.
async function acceptCombo(path, value) {
  let combo;
  try {
    combo = (await api("/api/hotkey/check", { value, role: roleOf(path) })).value;
  } catch (err) {
    showError(path, err.message);
    return;
  }
  if (!combo) return;
  const other = path === "continuous.hotkey" ? "dictation.hotkeys" : "continuous.hotkey";
  if (splitHotkeys(getPath(cfg(), other)).includes(combo)) {
    showError(path, tf("gui.hotkey.taken", { combo }));
    return;
  }
  const list = path === "continuous.hotkey" ? [] : splitHotkeys(getPath(cfg(), path));
  if (!list.includes(combo)) list.push(combo);
  await commit((c) => setPath(c, path, list.join(", ")), { field: path });
  redrawHotkeys();
}

// A non-modifier key completes the combination at once; a modifier-only one
// completes when its first modifier is released, with every modifier held.
function finishRecording(key) {
  const { path, mods } = rec;
  const value = [...MOD_ORDER.filter((m) => mods.has(m)), ...(key ? [key] : [])].join("+");
  endRecording();
  acceptCombo(path, value);
}

window.addEventListener("keydown", (ev) => {
  if (!rec) return;
  ev.preventDefault();
  ev.stopPropagation();
  // The OS recorder has the keyboard; Esc reaches it, not this page.
  if (ev.repeat || rec.mode === "os") return;
  const mod = modName(ev);
  if (mod) {
    rec.mods.add(mod);
    redrawHotkeys();
    return;
  }
  if (ev.code === "Escape" && rec.mods.size === 0) {
    endRecording();
    return;
  }
  const key = keyName(ev.code);
  if (!key) {
    showError(rec.path, tf("gui.hotkey.unrecordable", { code: ev.code || ev.key }));
    return;
  }
  // Modifiers held down before recording began only show up as flags.
  if (ev.ctrlKey) rec.mods.add("Ctrl");
  if (ev.altKey) rec.mods.add("Alt");
  if (ev.shiftKey) rec.mods.add("Shift");
  if (ev.metaKey) rec.mods.add("Super");
  if (ev.getModifierState?.("Fn")) rec.mods.add("Fn");
  finishRecording(key);
}, true);

window.addEventListener("keyup", (ev) => {
  if (!rec) return;
  ev.preventDefault();
  ev.stopPropagation();
  if (rec.mode === "keys" && modName(ev) && rec.mods.size) finishRecording(null);
}, true);

// Keys pressed in another app never reach the page; stop instead of leaving
// the global hotkeys paused. The OS recorder sees them, so it keeps going.
window.addEventListener("blur", () => { if (rec?.mode === "keys") endRecording(); });

// The yellow banner for hotkeys the system would not register.
function hotkeyBanner() {
  return el("div", { class: "banner warn hk-banner", role: "alert", hidden: true });
}

function drawHotkeyBanners() {
  for (const node of document.querySelectorAll(".hk-banner")) {
    node.hidden = !hotkeyWarning;
    if (!hotkeyWarning) continue;
    const open = isApp() && platform() === "macos"
      ? el("button", { type: "button", class: "ghost", onclick: () => api("/api/window", { action: "privacy" }).catch(() => {}) }, t("gui.open_system_settings"))
      : null;
    node.replaceChildren(el("span", {}, hotkeyWarning), open);
  }
}

// ---- 1. activity ----

const FILTERS = [
  ["all", "gui.activity.filter_all"],
  ["dictation", "gui.activity.filter_dictation"],
  ["continuous", "gui.activity.filter_continuous"],
];

function statCard(value, label) {
  return el("div", { class: "card stat" }, value, el("span", {}, label));
}

function buildActivity() {
  const sessions = el("b");
  const chars = el("b");
  const duration = el("b");
  const cards = el("div", { class: "cards" },
    statCard(sessions, t("gui.stats.sessions")),
    statCard(chars, t("gui.stats.chars")),
    statCard(duration, t("gui.stats.duration")));

  const gate = el("div", { class: "card gate", hidden: true },
    el("span", {}, t("gui.notready.text")),
    el("button", { type: "button", class: "main", onclick: () => showPage("engine") }, t("gui.notready.go")));

  let filter = "all";
  const open = new Set();
  let entries = [];
  const list = el("ol", { class: "log", "aria-live": "polite" });
  const empty = el("li", { class: "empty", hidden: true }, t("gui.activity.empty"));
  list.append(empty);
  const chips = segmented(FILTERS.map(([id, key]) => [id, t(key)]), filter, (id) => { filter = id; drawLog(); }, t("gui.activity.history"));

  // Rows are kept by entry: a new line slides in at the top, the others are
  // not touched. Only a new filter or language builds them again.
  const rows = new Map();
  let madeFor = null;
  let firstDraw = true;
  const makeRow = (k, e) => logRow(e, open, () => {
    const r = rows.get(k);
    const n = makeRow(k, e);
    r.node.replaceWith(n);
    r.node = n;
  });
  const keyed = (shown) => {
    const seen = new Map();
    return shown.map((e) => {
      const base = `${e.at}|${e.kind}|${e.text}`;
      const n = seen.get(base) ?? 0;
      seen.set(base, n + 1);
      return [n ? `${base}#${n}` : base, e];
    }).reverse();
  };
  const drawLog = () => {
    const shown = keyed(entries.filter((e) => filter === "all" || e.source === filter));
    const stamp = `${filter}|${LOCALE}`;
    if (stamp !== madeFor) {
      for (const r of rows.values()) r.node.remove();
      rows.clear();
      madeFor = stamp;
      firstDraw = true;
    }
    const wanted = new Set(shown.map(([k]) => k));
    for (const [k, r] of rows) {
      if (!wanted.has(k)) { r.node.remove(); rows.delete(k); }
    }
    let cursor = list.firstElementChild;
    for (const [k, e] of shown) {
      const r = rows.get(k);
      if (!r) {
        const node = makeRow(k, e);
        // (A slide-in for a new line is held back: add the `enter` class here.)
        list.insertBefore(node, cursor);
        rows.set(k, { node });
      } else if (r.node === cursor) {
        cursor = cursor.nextElementSibling;
      } else {
        list.insertBefore(r.node, cursor);
      }
    }
    setHidden(empty, shown.length > 0);
    firstDraw = false;
    if (jumpTo != null) showJumped(list);
  };

  live = (data) => {
    const n = (v) => v.toLocaleString(LOCALE);
    setText(sessions, n(data.stats.sessions));
    setText(chars, n(data.stats.chars));
    setText(duration, tf("gui.stats.minutes", { count: n(minutes(data.stats.audio_ms)) }));
    setHidden(gate, data.engine.ready);
    entries = data.activity;
    drawLog();
  };

  return [gate, cards, group(t("gui.activity.history"), [list], { action: chips })];
}

// The line the route page pointed at: scrolled to and lit for a moment.
function showJumped(list) {
  const session = jumpTo;
  jumpTo = null;
  const row = [...list.querySelectorAll("li[data-session]")].find((li) => li.dataset.session === session);
  if (!row) return;
  row.scrollIntoView({ block: "center", behavior: reducedMotion() ? "auto" : "smooth" });
  row.classList.add("flash");
  setTimeout(() => row.classList.remove("flash"), 1600);
}

function logRow(e, open, redraw) {
  const time = el("time", { datetime: new Date(e.at).toISOString() }, clock(e.at));
  if (e.kind === "debug") {
    return el("li", { class: "debug" }, el("time", {}, clockMs(e.at)),
      el("span", { class: "chip tag static debug" }, t("gui.activity.tag_debug")),
      el("span", { class: "text mono" }, e.text));
  }
  if (e.kind === "notice" || e.kind === "hook") {
    return el("li", { class: e.level }, time, el("span", { class: "text" }, e.text));
  }
  const tag = el("span", { class: "chip tag static" },
    t(e.source === "continuous" ? "gui.activity.tag_continuous" : "gui.activity.tag_dictation"));
  if (e.kind === "final") {
    const dest = e.engine ? tf("gui.activity.via", { engine: e.engine, note: e.note }) : e.note;
    return el("li", { class: "ok", "data-session": e.session ?? null }, time, tag,
      el("span", { class: "text" }, e.text), el("span", { class: "dest" }, dest), copyButton(e.text));
  }
  const expanded = open.has(e.at);
  const toggle = e.detail
    ? el("button", {
      type: "button",
      class: "ghost",
      onclick: () => { if (expanded) open.delete(e.at); else open.add(e.at); redraw(); },
    }, t(expanded ? "gui.activity.collapse" : "gui.activity.expand"))
    : el("span");
  return el("li", { class: "error" }, time, tag, el("span", { class: "text" }, e.text), el("span"), toggle,
    expanded && e.detail ? el("p", { class: "detail mono" }, e.detail) : null);
}

// ---- 1b. route ----
//
// Where each open session's sound comes from, the engine it goes to and
// where its text goes, as a map, with the latest messages under it. The map
// is drawn again only when its nodes change; levels, text and the packets
// moving along its lines are updated in place. Nothing here is saved.

const SVG_NS = "http://www.w3.org/2000/svg";
const ROUTE_FILTERS = [
  ["all", "gui.route.filter_all"],
  ["text", "gui.route.filter_text"],
  ["audio", "gui.route.filter_audio"],
  ["error", "gui.route.filter_error"],
];
const CHEVRON_ICON = ["M9 6l6 6-6 6"];
// The session the route page asked the Activity page to show.
let jumpTo = null;

function svgEl(tag, attrs = {}, ...children) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) if (v != null) node.setAttribute(k, v);
  node.append(...children.filter((c) => c != null));
  return node;
}

// Sets `node`'s text to `text`, cut to `width` px: from the end for live
// text (the newest words stay), from the start otherwise.
function fitText(node, text, width, { tail = false } = {}) {
  if (node.dataset.full === text && node.dataset.width === String(width)) return;
  node.dataset.full = text;
  node.dataset.width = String(width);
  node.textContent = text;
  let s = [...text];
  while (s.length > 1 && node.getComputedTextLength() > width) {
    s = tail ? s.slice(1) : s.slice(0, -1);
    node.textContent = tail ? `…${s.join("")}` : `${s.join("")}…`;
  }
}

function buildRoute() {
  const map = routeMap();
  const caption = el("p", { class: "route-caption", hidden: true }, t("gui.route.caption"));
  const card = el("div", { class: "card route-card" },
    el("div", { class: "route-head" }, t("gui.route.live")), map.node, caption);

  let filter = "all";
  let entries = [];
  const list = el("ol", { class: "log route-log" });
  const empty = el("li", { class: "empty", hidden: true }, t("gui.route.log_empty"));
  list.append(empty);
  const rows = new Map();
  let madeFor = null;
  const chips = segmented(ROUTE_FILTERS.map(([id, key]) => [id, t(key)]), filter,
    (id) => { filter = id; drawLog(); }, t("gui.route.log"));
  const drawLog = () => {
    const shown = entries.filter((e) => filter === "all" || e.filter === filter).reverse();
    if (filter !== madeFor) {
      for (const node of rows.values()) node.remove();
      rows.clear();
      madeFor = filter;
    }
    const key = (e) => `${e.seq}.${e.row}`;
    const wanted = new Set(shown.map(key));
    for (const [k, node] of rows) {
      if (!wanted.has(k)) { node.remove(); rows.delete(k); }
    }
    let cursor = list.firstElementChild;
    for (const e of shown) {
      const node = rows.get(key(e));
      if (!node) {
        const made = routeRow(e, map.pick);
        list.insertBefore(made, cursor);
        rows.set(key(e), made);
      } else if (node === cursor) {
        cursor = cursor.nextElementSibling;
      } else {
        list.insertBefore(node, cursor);
      }
    }
    setHidden(empty, shown.length > 0);
  };

  // The status poll paces the route's: one request at a time, only while
  // this page is shown.
  let asking = false;
  live = async () => {
    if (asking) return;
    asking = true;
    let data = null;
    try { data = await api("/api/route"); } catch {}
    asking = false;
    if (!data || !card.isConnected) return;
    map.draw(data.live);
    setHidden(caption, data.live.length === 0);
    entries = data.log;
    drawLog();
  };

  const note = el("span", { class: "route-note" }, t("gui.route.log_note"));
  return [card, group(t("gui.route.log"), [list], { action: el("span", { class: "route-actions" }, note, chips) })];
}

// One message: when, from where to where, what, and for the user's own
// dictation a way to its line on the Activity page.
function routeRow(e, pick) {
  const jump = e.activity
    ? el("button", {
      type: "button",
      class: "icon-btn",
      title: t("gui.route.open_activity"),
      "aria-label": t("gui.route.open_activity"),
      onclick: (ev) => { ev.stopPropagation(); jumpTo = e.session; showPage("activity"); },
    }, glyph(CHEVRON_ICON))
    : el("span");
  return el("li", { class: e.level || "plain", onclick: () => pick(e.session) },
    el("time", { datetime: new Date(e.at).toISOString() }, clockMs(e.at).slice(0, 8)),
    el("span", { class: "path", title: `${e.from} → ${e.to}` }, `${e.from} → ${e.to}`),
    el("span", { class: `kind ${e.kind}` }, e.label),
    el("span", { class: "text", title: e.detail }, e.detail),
    jump);
}

// The map of open sessions: sources on the left, where text goes on the
// right, Anya between them inside the local boundary, and the engines above
// it, outside. Hovering a node, or picking a message, lights its sessions'
// lines alone.
function routeMap() {
  const W = 700;
  const BOX = { w: 150, h: 30 };
  const ENGINE = { w: 204, h: 70, gap: 16 };
  const ROW = 48;
  // Anya's node: the app icon, 2R across, named under it.
  const R = 28;
  const anya = (cx, cy, on) => [
    svgEl("image", { class: "rm-anya-icon", href: "icon.svg", x: cx - R, y: cy - R, width: 2 * R, height: 2 * R }),
    // The icon's own rounded square sits 1/32 inside its image.
    svgEl("rect", { class: on ? "rm-anya on" : "rm-anya", x: cx - R * 15 / 16, y: cy - R * 15 / 16, width: R * 15 / 8, height: R * 15 / 8, rx: 12 }),
    svgEl("text", { class: "rm-anya-label rm-small", x: cx, y: cy + R + 15 }, t("gui.route.anya")),
  ];
  const node = el("div", { class: "route-map" });
  let shape = null;
  let svg = null;
  // Per drawing: engines by id, links, nodes, and what each session last showed.
  let parts = null;
  let picked = null;
  let pickTimer = 0;
  // What each session last sent a packet for, kept across redraws.
  const seen = new Map();
  const packets = [];
  let frame = 0;

  const light = (sessions) => {
    if (!svg) return;
    toggleClass(svg, "focus", !!sessions);
    for (const item of svg.querySelectorAll("[data-sessions]")) {
      const mine = sessions && item.dataset.sessions.split(" ").some((s) => sessions.includes(s));
      toggleClass(item, "lit", !!mine);
    }
  };
  const hoverable = (item, sessions) => {
    item.dataset.sessions = sessions.join(" ");
    item.addEventListener("mouseenter", () => light(sessions));
    item.addEventListener("mouseleave", () => light(picked ? [picked] : null));
    return item;
  };

  const idle = () => {
    svg = svgEl("svg", { viewBox: `0 0 ${W} 150`, class: "route-svg idle", role: "img", "aria-label": t("gui.route.idle") },
      svgEl("rect", { class: "rm-bound", x: 1, y: 1, width: W - 2, height: 148, rx: 12 }),
      svgEl("text", { class: "rm-small rm-muted", x: 16, y: 22 }, t("gui.route.boundary")),
      ...anya(W / 2, 62, false),
      svgEl("text", { class: "rm-small rm-muted", x: W / 2, y: 132, "text-anchor": "middle" }, t("gui.route.idle")));
    node.replaceChildren(svg);
    parts = null;
  };

  const build = (routes) => {
    const sources = new Map();
    const engines = new Map();
    const sinks = new Map();
    for (const r of routes) {
      const s = sources.get(r.origin.key) ?? { ...r.origin, sessions: [] };
      s.sessions.push(r.session);
      sources.set(r.origin.key, s);
      const e = engines.get(r.engine.id) ?? { ...r.engine, roles: new Set(), streaming: r.streaming, sessions: [] };
      e.roles.add(r.origin.role);
      e.sessions.push(r.session);
      engines.set(r.engine.id, e);
      for (const k of r.sinks) {
        const d = sinks.get(k.key) ?? { ...k, sessions: [] };
        d.sessions.push(r.session);
        sinks.set(k.key, d);
      }
    }
    const n = Math.max(sources.size, sinks.size, 1);
    const boundTop = ENGINE.h + 32;
    const rowsTop = boundTop + 34;
    const cy = rowsTop + (n * ROW) / 2;
    const cx = W / 2;
    const height = Math.max(rowsTop + n * ROW + 12, cy + R + 26);
    const column = (count) => (i) => rowsTop + ROW * (i + (n - count) / 2) + ROW / 2;

    svg = svgEl("svg", { viewBox: `0 0 ${W} ${height + 2}`, class: "route-svg" });
    const linkLayer = svgEl("g");
    const packetLayer = svgEl("g", { class: "rm-packets" });
    const nodeLayer = svgEl("g");
    svg.append(svgEl("rect", { class: "rm-bound", x: 1, y: boundTop, width: W - 2, height: height - boundTop, rx: 12 }),
      svgEl("text", { class: "rm-small rm-muted", x: 16, y: boundTop + 20 }, t("gui.route.boundary")),
      linkLayer, packetLayer, nodeLayer);
    node.replaceChildren(svg);

    const link = (d, sessions) => {
      const path = hoverable(svgEl("path", { class: "rm-link", d }), sessions);
      linkLayer.append(path);
      return path;
    };
    const toAnya = new Map();
    const fromAnya = new Map();
    const toEngine = new Map();

    const ex = ENGINE.w * engines.size + ENGINE.gap * (engines.size - 1);
    [...engines.values()].forEach((e, j) => {
      const x = (W - ex) / 2 + j * (ENGINE.w + ENGINE.gap);
      const ecx = x + ENGINE.w / 2;
      const sx = cx + (j - (engines.size - 1) / 2) * 14;
      const sy = cy - R + 2;
      toEngine.set(e.id, link(`M${sx} ${sy} C${sx} ${sy - 44} ${ecx} ${ENGINE.h + 44} ${ecx} ${ENGINE.h}`, e.sessions));
      const title = svgEl("text", { class: "rm-title", x: x + 12, y: 20 });
      const roles = svgEl("text", { class: "rm-small rm-muted", x: x + 12, y: 36 });
      const bars = BAR_WEIGHTS.slice(0, 5).map((_, i) => svgEl("rect", { class: "rm-bar", x: x + 12 + i * 5, y: 52, width: 3, height: 2, rx: 1 }));
      const text = svgEl("text", { class: "rm-small", x: x + 44, y: 57 });
      nodeLayer.append(hoverable(svgEl("g", { class: "rm-node" },
        svgEl("rect", { class: "rm-box", x, y: 0, width: ENGINE.w, height: ENGINE.h, rx: 10 }),
        title, roles, ...bars, text), e.sessions));
      const how = t(e.streaming ? "gui.route.streaming" : "gui.route.batch");
      fitText(roles, `${[...e.roles].join(" · ")} · ${how}`, ENGINE.w - 24);
      fitText(title, e.title, ENGINE.w - 24);
      e.bars = bars;
      e.text = text;
      e.x = x;
    });

    const box = (x, y, label, sessions) => {
      const labelNode = svgEl("text", { class: "rm-label", x: x + 12, y: y + 5 });
      nodeLayer.append(hoverable(svgEl("g", { class: "rm-node" },
        svgEl("rect", { class: "rm-box", x, y: y - BOX.h / 2, width: BOX.w, height: BOX.h, rx: 8 }),
        svgEl("title", {}, label), labelNode), sessions));
      fitText(labelNode, label, BOX.w - 24);
    };
    const sy = column(sources.size);
    [...sources.values()].forEach((s, i) => {
      const y = sy(i);
      toAnya.set(s.key, link(`M${16 + BOX.w} ${y} C${cx - 110} ${y} ${cx - 110} ${cy} ${cx - R} ${cy}`, s.sessions));
      box(16, y, s.label, s.sessions);
    });
    const dy = column(sinks.size);
    [...sinks.values()].forEach((d, i) => {
      const y = dy(i);
      const x = W - 16 - BOX.w;
      fromAnya.set(d.key, link(`M${cx + R} ${cy} C${cx + 110} ${cy} ${cx + 110} ${y} ${x} ${y}`, d.sessions));
      box(x, y, d.label, d.sessions);
    });
    nodeLayer.append(hoverable(svgEl("g", { class: "rm-node" }, ...anya(cx, cy, true)), routes.map((r) => r.session)));

    parts = { engines, toAnya, fromAnya, toEngine, packetLayer };
    light(picked ? [picked] : null);
  };

  // Packets: a few bars for audio going up to the engine, a pill of the
  // newest words coming back down and out.
  const run = (paths, make, durs) => {
    if (!parts || reducedMotion()) return;
    const item = make();
    parts.packetLayer.append(item);
    packets.push({ item, paths, durs, leg: 0, start: performance.now() });
    if (!frame) frame = requestAnimationFrame(step);
  };
  const step = (now) => {
    frame = 0;
    for (let i = packets.length - 1; i >= 0; i--) {
      const p = packets[i];
      let k = (now - p.start) / p.durs[p.leg];
      if (k >= 1 && p.leg + 1 < p.paths.length) {
        p.leg += 1;
        p.start = now;
        k = 0;
      }
      if (k >= 1 || !p.item.isConnected) {
        p.item.remove();
        packets.splice(i, 1);
        continue;
      }
      const [path, backwards] = p.paths[p.leg];
      const len = path.getTotalLength();
      const at = path.getPointAtLength(len * (backwards ? 1 - k : k));
      p.item.setAttribute("transform", `translate(${at.x.toFixed(1)} ${at.y.toFixed(1)})`);
    }
    if (packets.length && node.isConnected) frame = requestAnimationFrame(step);
  };
  const audioGlyph = () => svgEl("g", { class: "rm-audio" },
    svgEl("rect", { x: -4, y: -3, width: 2, height: 6, rx: 1 }),
    svgEl("rect", { x: -1, y: -5, width: 2, height: 10, rx: 1 }),
    svgEl("rect", { x: 2, y: -2, width: 2, height: 4, rx: 1 }));
  const textGlyph = (words) => {
    const label = svgEl("text", { class: "rm-pill-text", x: 0, y: 3.5 }, words);
    return svgEl("g", { class: "rm-text" }, svgEl("rect", { x: -16, y: -7, width: 32, height: 14, rx: 7 }), label);
  };

  const update = (routes) => {
    if (!parts) return;
    const now = performance.now();
    const levels = new Map();
    for (const s of seen.keys()) {
      if (!routes.some((r) => r.session === s)) seen.delete(s);
    }
    for (const r of routes) {
      const e = parts.engines.get(r.engine.id);
      levels.set(r.engine.id, Math.max(levels.get(r.engine.id) ?? 0, r.level));
      fitText(e.text, r.text, ENGINE.w - 56, { tail: true });
      const last = seen.get(r.session) ?? { text: "", audio: 0 };
      const up = [parts.toAnya.get(r.origin.key), parts.toEngine.get(r.engine.id)];
      if (r.level > 0.04 && now - last.audio > 900 - r.level * 500) {
        last.audio = now;
        run([[up[0], false], [up[1], false]], audioGlyph, [550, 450]);
      }
      if (r.text && r.text !== last.text) {
        last.text = r.text;
        const words = [...r.text.replace(/[\s\p{P}]+$/u, "")].slice(-2).join("");
        for (const k of r.sinks) {
          run([[up[1], true], [parts.fromAnya.get(k.key), false]], () => textGlyph(words), [450, 550]);
        }
      }
      seen.set(r.session, last);
    }
    for (const [id, e] of parts.engines) {
      const shaped = Math.min(1, (levels.get(id) ?? 0) ** 0.6 * 1.4);
      e.bars.forEach((bar, i) => {
        const h = Math.max(2, Math.round(14 * shaped * BAR_WEIGHTS[i]));
        bar.setAttribute("height", h);
        bar.setAttribute("y", 53 - h / 2);
      });
    }
  };

  const draw = (routes) => {
    const key = JSON.stringify(routes.map((r) => [r.session, r.origin.key, r.origin.label, r.engine.id, r.streaming, r.sinks]));
    if (key !== shape) {
      shape = key;
      if (routes.length) build(routes); else idle();
    }
    update(routes);
  };

  // A message picked in the log lights its session for a moment.
  const pick = (session) => {
    picked = session;
    light([session]);
    clearTimeout(pickTimer);
    pickTimer = setTimeout(() => { picked = null; light(null); }, 2000);
  };

  return { node, draw, pick };
}

// ---- 2. dictation ----

function buildDictation() {
  const hotkeys = row(t("gui.hotkey.label"), t("gui.hotkey.help"), hotkeyField("dictation.hotkeys"),
    { error: "dictation.hotkeys", top: true });
  const modeHelp = () => t(cfg().dictation.mode === "hold" ? "gui.mode.help_hold" : "gui.mode.help_toggle");
  const delay = row(t("gui.stop_delay.label"), t("gui.stop_delay.help"), slider({
    min: 0, max: 1000, step: 50, value: cfg().dictation.stop_delay_ms,
    format: (ms) => tf("gui.stop_delay.value", { ms }),
    aria: t("gui.stop_delay.label"),
    onchange: (ms, back) => commit((c) => { c.dictation.stop_delay_ms = ms; }, { field: "dictation.stop_delay_ms", revert: () => back(cfg().dictation.stop_delay_ms) }),
  }));
  delay.hidden = cfg().dictation.mode !== "hold";
  const modeSeg = segmented(
    [["toggle", t("gui.mode.toggle")], ["hold", t("gui.mode.hold")]],
    cfg().dictation.mode,
    (value) => {
      // The row follows the choice at once; a refusal puts it back.
      const sync = (v) => { mode.help.textContent = t(v === "hold" ? "gui.mode.help_hold" : "gui.mode.help_toggle"); delay.hidden = v !== "hold"; };
      sync(value);
      commit((c) => { c.dictation.mode = value; }, {
        field: "dictation.mode",
        revert: () => { modeSeg.set(cfg().dictation.mode); sync(cfg().dictation.mode); },
      });
    },
    t("gui.mode.label"));
  const mode = row(t("gui.mode.label"), modeHelp(), modeSeg);
  const outputSeg = segmented(
    [["paste", t("gui.output.paste")], ["clipboard", t("gui.output.clipboard")], ["none", t("gui.output.none")]],
    cfg().dictation.output,
    (value) => commit((c) => { c.dictation.output = value; }, { field: "dictation.output", revert: () => outputSeg.set(cfg().dictation.output) }),
    t("gui.output.label"));
  const output = row(t("gui.output.label"), t("gui.output.help"), outputSeg);
  return [
    hotkeyBanner(),
    group(t("gui.hotkey.label"), [
      row(t("gui.dictation.enabled"), null, bindToggle("dictation.enabled", t("gui.dictation.enabled"))),
      hotkeys, mode, delay]),
    group(t("gui.dictation.group_input"), [row(t("gui.device.label"), null, deviceSelect("dictation.device")), output]),
  ];
}

// The microphones, the system default first. A configured device that is not
// plugged in now stays selectable, so opening the settings never rewrites
// the config.
function microphoneOptions(value) {
  const current = /^default$/i.test(value) ? "" : value;
  const mics = state.devices.filter((d) => d.kind === "microphone");
  const options = [{ value: "", label: t("gui.device.default") }, ...mics.map((d) => ({ value: d.id, label: d.name }))];
  const known = state.devices.find((d) => d.id === current || d.name === current);
  if (current && !known) options.push({ value: current, label: tf("gui.device.absent", { name: current }) });
  return { options, selected: known && mics.includes(known) ? known.id : current };
}

function deviceSelect(path) {
  const { options, selected } = microphoneOptions(getPath(cfg(), path));
  const select = selectBox(options, selected, (value) => commit((c) => setPath(c, path, value), { field: path }), t("gui.device.label"));
  const box = ctl(select);
  if (state.devices_error) box.append(el("span", { class: "field-error" }, state.devices_error));
  return box;
}

// ---- 3. continuous ----

function buildContinuous() {
  let busy = false;
  const run = el("input", { type: "checkbox", class: "switch", "aria-label": t("gui.continuous.run") });
  run.checked = cfg().continuous.enabled;
  run.addEventListener("change", async () => {
    busy = true;
    try {
      const res = await api("/api/continuous", { on: run.checked });
      applyState(res.state, { render: false });
      hotkeyWarning = res.warning ?? null;
      drawHotkeyBanners();
    } catch (err) {
      run.checked = !run.checked;
      notify(err.message, "error");
    }
    busy = false;
  });
  const runRow = row(t("gui.continuous.run"), t("gui.continuous.stopped"), ctl(run));

  live = (data) => {
    const on = data.status.continuous_on;
    if (!busy && run.checked !== on) run.checked = on;
    setText(runRow.help, on
      ? tf("gui.continuous.running", { source: sourceLabel(cfg().continuous.device), count: data.subscription.subscribers ?? 0 })
      : t("gui.continuous.stopped"));
  };

  const sourceRow = row(t("gui.continuous.device"), t("gui.continuous.device_help"), ctl(sourceDropdown()), { top: true });
  const label = sourceRow.querySelector(".label");
  if (platform() === "macos") label.append(el("p", { class: "note warn" }, t("gui.continuous.permission_note")));
  if (state.devices_error) label.append(el("p", { class: "field-error" }, state.devices_error));

  return [
    hotkeyBanner(),
    group(null, [
      runRow,
      row(t("gui.continuous.hotkey"), null, hotkeyField("continuous.hotkey"), { error: "continuous.hotkey", top: true })]),
    group(t("gui.continuous.group_source"), [
      sourceRow,
      row(t("gui.continuous.silence"), t("gui.continuous.silence_help"), bindToggle("continuous.silence_gate", t("gui.continuous.silence")))],
    { note: t("gui.continuous.footer") }),
  ];
}

// What the continuous source is called: system sound, or a device by name.
function sourceLabel(device) {
  const id = /^default$/i.test(device) ? "" : device;
  if (!id) return t("gui.continuous.source_system");
  return state.devices.find((d) => d.id === id || d.name === id)?.name ?? id;
}

function sourceDropdown() {
  const button = el("button", { type: "button", "aria-haspopup": "listbox", "aria-expanded": "false" });
  const wrap = el("div", { class: "drop" }, button);
  let menu = null;
  const current = () => cfg().continuous.device;
  const label = (device) => el("span", {}, sourceLabel(device));
  const items = () => [...(menu?.querySelectorAll("button") ?? [])];
  const close = (refocus) => {
    if (!menu) return;
    const gone = menu;
    menu = null;
    document.removeEventListener("click", outside, true);
    button.setAttribute("aria-expanded", "false");
    gone.classList.add("closing");
    setTimeout(() => gone.remove(), reducedMotion() ? 0 : 80);
    if (refocus) button.focus({ preventScroll: true });
  };
  const outside = (ev) => { if (!wrap.contains(ev.target)) close(false); };
  const open = () => {
    menu = draw();
    wrap.append(menu);
    button.setAttribute("aria-expanded", "true");
    document.addEventListener("click", outside, true);
    (menu.querySelector('[aria-selected="true"]') ?? items()[0])?.focus({ preventScroll: true });
  };
  const pick = (value) => {
    close(true);
    // The label follows the choice at once; a refusal puts it back.
    button.replaceChildren(label(value));
    commit((c) => { c.continuous.device = value; }, {
      field: "continuous.device",
      revert: () => button.replaceChildren(label(current())),
    });
  };
  const item = (value, text, sub) => el("button", {
    type: "button",
    role: "option",
    "aria-selected": String(value === (/^default$/i.test(current()) ? "" : current())),
    onclick: () => pick(value),
  }, text, sub ? el("small", {}, sub) : null);
  const draw = () => {
    const loops = state.devices.filter((d) => d.kind === "loopback");
    const mics = state.devices.filter((d) => d.kind === "microphone");
    const known = state.devices.some((d) => d.id === current() || d.name === current());
    const extra = current() && !/^default$/i.test(current()) && !known
      ? [item(current(), tf("gui.device.absent", { name: current() }))] : [];
    return el("div", { class: "menu", role: "listbox" },
      el("h3", {}, t("gui.device.group_loopback")),
      item("", t("gui.continuous.source_system"), t("gui.continuous.source_system_hint")),
      ...loops.map((d) => item(d.id, d.name)),
      mics.length || extra.length ? el("h3", {}, t("gui.device.group_microphone")) : null,
      ...mics.map((d) => item(d.id, d.name)),
      ...extra);
  };
  button.replaceChildren(label(current()));
  button.addEventListener("click", () => { if (menu) close(false); else open(); });
  wrap.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && menu) { ev.preventDefault(); ev.stopPropagation(); close(true); return; }
    const step = { ArrowDown: 1, ArrowUp: -1 }[ev.key];
    if (!step) return;
    ev.preventDefault();
    if (!menu) { open(); return; }
    const all = items();
    const at = all.indexOf(document.activeElement);
    all[(at + step + all.length) % all.length]?.focus({ preventScroll: true });
  });
  return wrap;
}

// ---- 4. engine ----
//
// Several recognition services can be set up at once. The first group lists
// every one, its form folded under it; the second picks, for each use, the
// one service that use sends its audio to.

const ROLES = [
  ["dictation", "gui.engine.use_dictation", "gui.engine.use_dictation_hint"],
  ["continuous", "gui.engine.use_continuous", "gui.engine.use_continuous_hint"],
  ["remote", "gui.engine.use_remote", "gui.engine.use_remote_hint"],
];
const TILES = ["#3c6cf0", "#ff6a00", "#10a37f", "#7d5cff", "#ff6900", "#34c759", "#0078d4", "#d6336c"];

// The services whose form is unfolded, kept across redraws of the page.
const openEngines = new Set();
// Repaints what depends on which services are usable; set by the engine page.
let engineViews = null;

// The engine's or plugin's icon when it has one that loads; else a coloured tile with
// the name's first letter, the same colour every time.
function tileFor(id, title, icon) {
  if (icon) {
    const img = el("img", { class: "tile icon", src: icon, alt: "", "aria-hidden": "true" });
    img.addEventListener("error", () => img.replaceWith(tileFor(id, title)), { once: true });
    return img;
  }
  let h = 0;
  for (const ch of id) h = (h * 31 + ch.codePointAt(0)) >>> 0;
  const letter = ([...title.trim()][0] ?? "?").toUpperCase();
  const tile = el("span", { class: "tile", "aria-hidden": "true" }, letter);
  // Through the CSSOM: the page's policy forbids style attributes.
  tile.style.background = TILES[h % TILES.length];
  return tile;
}

// A row that acts like a button: Enter and Space press it.
function pressable(node, action) {
  node.setAttribute("role", "button");
  node.tabIndex = 0;
  node.addEventListener("click", action);
  node.addEventListener("keydown", (ev) => {
    if (ev.target === node && (ev.key === "Enter" || ev.key === " ")) { ev.preventDefault(); action(); }
  });
  return node;
}

// Whether a service can take a session now: it is on and set up.
function usable(e) {
  return !e.custom && e.enabled !== false && (ready[e.id] ?? e.ready);
}

function buildEngine() {
  const paints = [];
  const serviceRows = state.engines.map((e) => serviceRow(e.id, paints));
  const add = el("div", { class: "prov" }, pressable(el("div", { class: "prov-head" },
    el("span", { class: "tile add", "aria-hidden": "true" }, "+"),
    el("div", { class: "svc" }, el("b", {}, t("gui.engine.more_plugins")), el("small", {}, t("gui.engine.add_hint"))),
    el("span", { class: "disc", "aria-hidden": "true" }, "›")), () => showPage("plugins")));
  const services = group(t("gui.engine.group_engines"), [...serviceRows, add]);

  const uses = group(t("gui.engine.group_uses"), [], { note: t("gui.engine.uses_note") });
  const box = uses.querySelector(".group");
  let usesKey = null;
  const drawUses = () => {
    const options = state.engines.filter(usable).map((e) => ({ value: e.id, label: e.title }));
    const key = JSON.stringify([options, ROLES.map(([role]) => cfg().recognition[role]), LOCALE]);
    if (key === usesKey) return;
    usesKey = key;
    box.replaceChildren(...ROLES.map(([role, label, hint]) => {
      const id = cfg().recognition[role];
      const current = state.engines.find((e) => e.id === id);
      const title = current?.title ?? id;
      const fine = options.some((o) => o.value === id);
      // A choice that cannot run stays in the list, so it is seen, with a note.
      const select = selectBox(fine ? options : [{ value: id, label: title }, ...options], id, (value) => commit((c) => {
        c.recognition[role] = value;
      }, {
        field: `recognition.${role}`,
        revert: () => { select.value = cfg().recognition[role]; },
      }).then(drawUses), t(label));
      const line = row(t(label), t(hint), ctl(select), { error: `recognition.${role}` });
      if (!fine) line.help.after(el("small", { class: "note warn" }, tf("gui.engine.choice_not_ready", { engine: title })));
      return line;
    }));
  };
  engineViews = {
    paint: () => {
      for (const paint of paints) paint();
      drawUses();
      drawEngineLine();
    },
  };
  drawUses();

  return [
    services,
    uses,
    group(t("gui.engine.group_recognition"), [
      row(t("gui.language.label"), t("gui.language.help"), ctl(bindSelect("recognition.language", state.languages, t("gui.language.label")))),
      row(t("gui.hotwords.label"), t("gui.hotwords.help"), ctl(hotwordsField()), { stack: true }),
      row(t("gui.hotwords_file.label"), t("gui.hotwords_file.help"), pathLine(state.info.hotwords_path), { stack: true })]),
  ];
}

// One recognition service: its summary, and unfolded the form its manifest
// describes. `paints` collects what repaints its state.
function serviceRow(id, paints) {
  const engine = state.engines.find((e) => e.id === id);
  const status = el("span");
  const paint = () => {
    const e = state.engines.find((x) => x.id === id);
    if (!e) return;
    const ok = usable(e);
    status.className = ok ? "status-ok" : "status-no";
    status.textContent = e.custom ? t("gui.engine.unavailable") : e.enabled === false ? t("gui.engine.off") : ok ? `● ${t("gui.engine.ready")}` : t("gui.engine.unconfigured");
  };
  paints.push(paint);
  paint();

  const head = el("div", { class: "prov-head" },
    tileFor(id, engine.title, engine.icon),
    el("div", { class: "svc" }, el("b", { title: engine.title }, engine.title), el("small", {}, engine.blurb)),
    engine.plugin ? el("span", { class: "chip tag static plug" }, t("gui.engine.plugin_tag")) : null,
    engine.custom ? null : el("span", { class: "chip tag static" }, t(engine.streaming ? "gui.engine.streaming" : "gui.engine.batch")),
    status,
    engine.custom ? null : el("span", { class: "disc", "aria-hidden": "true" }, "›"));
  const wrap = el("div", { class: "prov", "data-engine": id }, head);
  if (engine.custom) return wrap;

  const inner = engineForm(engine, () => engineViews?.paint(), paints);
  wrap.append(el("div", { class: "fields" }, inner));
  const fold = (open) => {
    wrap.classList.toggle("open", open);
    head.setAttribute("aria-expanded", String(open));
    if (open) openEngines.add(id); else openEngines.delete(id);
  };
  fold(openEngines.has(id));
  pressable(head, () => {
    fold(!wrap.classList.contains("open"));
    if (wrap.classList.contains("open")) setTimeout(() => inner.querySelector("input[type=text], input[type=password]")?.focus(), reducedMotion() ? 0 : 200);
  });
  return wrap;
}

function openDoc(url) {
  if (isApp()) api("/api/open", { url }).catch((err) => notify(err.message, "error"));
  else window.open(url, "_blank", "noopener");
}

function fieldInput(f, value, oninput, onchange) {
  if (f.kind === "choice") {
    // A configured value outside the options stays selectable.
    const values = !value || f.options.includes(value) ? f.options : [...f.options, value];
    const select = el("select", { "aria-label": f.label, onchange: () => onchange(select.value) },
      el("option", { value: "" }, t("gui.engine.default")),
      ...values.map((o) => el("option", { value: o }, o)));
    select.value = value;
    return select;
  }
  const input = el("input", {
    type: f.kind === "secret" ? "password" : "text",
    spellcheck: "false",
    autocomplete: "off",
    placeholder: t(f.required ? "gui.engine.required" : "gui.engine.default"),
    "aria-label": f.label,
  });
  input.value = value;
  input.addEventListener("input", () => oninput(input.value));
  input.addEventListener("change", () => onchange(input.value));
  if (f.kind !== "secret") return input;
  const show = el("button", {
    type: "button",
    class: "ghost",
    onclick: () => {
      const reveal = input.type === "password";
      input.type = reveal ? "text" : "password";
      show.textContent = t(reveal ? "gui.engine.hide" : "gui.engine.show");
    },
  }, t("gui.engine.show"));
  return el("span", { class: "secret" }, input, show);
}

// The form of one service, generated from its manifest: the built-in's
// switch, the fields (advanced ones folded), a button to its documentation.
// `repaint` runs when what the form holds changes whether it can run;
// `paints` collects what puts the form's own controls right again.
function engineForm(engine, repaint, paints) {
  const saved = () => cfg().recognition.engines[engine.id] ?? {};
  const values = typed[engine.id] ?? Object.fromEntries(engine.fields.map((f) => [f.key, saved()[f.key] ?? ""]));
  typed[engine.id] = values;
  let timer = null;
  const settings = () => {
    // Keys the manifest does not list are kept as they are.
    const next = { ...saved() };
    for (const f of engine.fields) {
      const v = String(values[f.key]).trim();
      if (v) next[f.key] = v;
      else delete next[f.key];
    }
    return next;
  };
  const recheck = () => {
    typed[engine.id] = values;
    clearTimeout(timer);
    repaint();
    timer = setTimeout(async () => {
      try {
        ready[engine.id] = (await api("/api/engine/ready", { id: engine.id, settings: settings() })).ready;
      } catch {
        return;
      }
      repaint();
    }, 250);
  };
  const line = (label, control, help) => el("div", { class: "field" },
    el("span", { class: "fl" }, label),
    el("span", { class: "fv" }, control, help ? el("small", {}, help) : null));
  const fieldRow = (f) => line(
    el("span", {}, f.label, f.required && !f.advanced ? el("span", { class: "req" }, "*") : null),
    fieldInput(f, values[f.key], (v) => { values[f.key] = v; recheck(); }, (v) => {
      values[f.key] = v;
      const next = settings();
      commit((c) => {
        if (Object.keys(next).length || c.recognition.engines[engine.id]) c.recognition.engines[engine.id] = next;
        else delete c.recognition.engines[engine.id];
      }, { field: "recognition.engines" }).then(() => { recheck(); });
    }),
    f.help || null);

  const rows = [];
  if (engine.enabled !== null && engine.enabled !== undefined) {
    // A built-in's switch lives here. The service a use is set to stays on.
    const sw = el("input", { type: "checkbox", class: "switch", "aria-label": tf("gui.engine.switch", { engine: engine.title }) });
    const sync = () => {
      const now = state.engines.find((e) => e.id === engine.id);
      const chosen = ROLES.some(([role]) => cfg().recognition[role] === engine.id);
      sw.checked = (now?.enabled ?? true) || chosen;
      sw.disabled = chosen;
      sw.title = chosen ? t("gui.engine.switch_chosen") : "";
    };
    sw.addEventListener("change", async () => {
      await commit((c) => { c.plugins.builtin = { ...(c.plugins.builtin ?? {}), [engine.id]: sw.checked }; }, { revert: sync });
      sync();
      repaint();
    });
    sync();
    paints.push(sync);
    rows.push(line(t("gui.engine.enabled"), sw, t("gui.engine.enabled_help")));
  }
  const main = engine.fields.filter((f) => !f.advanced);
  const advanced = engine.fields.filter((f) => f.advanced);
  rows.push(...main.map(fieldRow));
  if (advanced.length) {
    // Open when it holds something, so a saved value is never folded away.
    rows.push(el("details", { class: "advanced", open: main.length === 0 || advanced.some((f) => String(values[f.key]).trim()) },
      el("summary", {}, tf("gui.engine.advanced", { count: advanced.length })),
      el("div", { class: "adv-fields" }, ...advanced.map(fieldRow))));
  }
  if (!engine.fields.length && !rows.length) rows.push(el("p", { class: "note" }, t("gui.engine.no_fields")));
  rows.push(errorSlot("recognition.engines"));
  if (engine.docs) {
    rows.push(el("div", { class: "docs-line" },
      el("button", { type: "button", class: "ghost", onclick: () => openDoc(engine.docs) }, t("gui.engine.docs"))));
  }
  return el("div", { class: "inner" }, ...rows);
}

function hotwordsField() {
  const box = el("div", { class: "chips" });
  const words = () => cfg().recognition.hotwords;
  const save = (list) => commit((c) => { c.recognition.hotwords = list; }, { field: "recognition.hotwords" }).then(draw);
  const add = (text) => {
    const list = [...words()];
    for (const w of text.split(/[,，、\n]/).map((s) => s.trim()).filter(Boolean)) if (!list.includes(w)) list.push(w);
    if (list.length !== words().length) save(list);
    else draw();
  };
  function draw() {
    box.replaceChildren(
      ...words().map((w, i) => el("span", { class: "chip" }, w, el("button", {
        type: "button",
        title: tf("gui.hotwords.remove", { word: w }),
        "aria-label": tf("gui.hotwords.remove", { word: w }),
        onclick: () => save(words().filter((_, j) => j !== i)),
      }, "×"))),
      el("button", {
        type: "button",
        class: "chip add",
        onclick: (ev) => {
          const input = el("input", { type: "text", spellcheck: "false", placeholder: t("gui.hotwords.placeholder"), "aria-label": t("gui.hotwords.label") });
          let done = false;
          const finish = () => { if (!done) { done = true; add(input.value); } };
          input.addEventListener("keydown", (e) => {
            if (e.isComposing) return; // IME candidate selection
            if (e.key === "Enter" || e.key === "," || e.key === "，" || e.key === "、") { e.preventDefault(); finish(); }
            else if (e.key === "Escape") { done = true; draw(); }
          });
          input.addEventListener("blur", finish);
          ev.currentTarget.replaceWith(el("span", { class: "chip" }, input));
          input.focus();
        },
      }, t("gui.hotwords.add")));
  }
  draw();
  return box;
}


// ---- 5. plugins ----
//
// Installing and enabling are separate steps: an enabled plugin hears the
// microphone of every session opened on its engine.

let plugins = [];
let marketState = { status: "loading", list: [], reason: "" };
let installing = null;
// The plugin just installed: its row is highlighted when it first appears.
let freshPlugin = null;
let pluginViews = null;
let pluginError = null;

// The words of each kind a plugin provides, as the server names it.
const PROVIDE_NAME = {
  engine: "gui.provide.engine",
  context: "gui.provide.context",
  transform: "gui.provide.transform",
  action: "gui.provide.action",
};
const PROVIDE_GETS = {
  engine: "gui.plugins.gets_engine",
  context: "gui.plugins.gets_context",
  transform: "gui.plugins.gets_transform",
  action: "gui.plugins.gets_action",
};
const HOOK_RUNS = {
  builtin: "gui.hooks.runs_builtin",
  inline: "gui.hooks.runs_inline",
  host: "gui.hooks.runs_host",
};

// What a plugin's kinds receive, then where it runs and what it asks for:
// the words of the install and enable consent, and the lines of an installed
// row. A plugin that has not loaded yet lists no kinds.
function disclosure(p) {
  const kinds = p.provides.length
    ? p.provides.map((k) => el("li", {}, t(PROVIDE_GETS[k])))
    : [el("li", {}, t("gui.plugins.provides_none"))];
  return [
    ...kinds,
    el("li", {}, t(p.host ? "gui.plugins.runs_host" : "gui.plugins.runs_inline")),
    ...(p.host ? [el("li", {}, tf("gui.plugins.permissions", {
      list: p.permissions.length ? p.permissions.join(", ") : t("gui.plugins.no_permissions"),
    }))] : []),
  ];
}

function provideChips(p) {
  return p.provides.map((k) => el("span", { class: "chip tag static" }, t(PROVIDE_NAME[k])));
}

// The hook settings of one plugin, saved under `[plugins.settings.<id>]`.
function pluginSettings(p) {
  const saved = () => cfg().plugins.settings?.[p.id] ?? {};
  const values = Object.fromEntries(p.fields.map((f) => [f.key, saved()[f.key] ?? ""]));
  const collect = () => {
    const next = {};
    for (const f of p.fields) {
      const v = String(values[f.key]).trim();
      if (v) next[f.key] = v;
    }
    return next;
  };
  const rows = p.fields.map((f) => el("div", { class: "field" },
    el("span", { class: "fl" }, f.label),
    el("span", { class: "fv" },
      fieldInput(f, values[f.key], (v) => { values[f.key] = v; }, (v) => {
        values[f.key] = v;
        const next = collect();
        commit((c) => {
          c.plugins.settings = c.plugins.settings ?? {};
          if (Object.keys(next).length) c.plugins.settings[p.id] = next;
          else delete c.plugins.settings[p.id];
        }, { field: "plugins.settings" });
      }),
      f.help ? el("small", {}, f.help) : null)));
  // A plugin waiting for a required setting opens on it.
  return el("details", { class: "plugin-settings", open: p.enabled && p.missing.length > 0 },
    el("summary", {}, t("gui.plugins.settings")),
    ...rows,
    errorSlot("plugins.settings"));
}

function installedItem(p) {
  const row = el("div", { class: p.id === freshPlugin ? "row plugin fresh" : "row plugin" },
    tileFor(p.id, p.name, p.icon),
    el("div", { class: "svc" },
      el("b", { title: p.name }, p.name, el("span", { class: "ver" }, `v${p.version}`)),
      el("small", { class: p.enabled ? `state-${p.state}` : "" }, p.enabled ? p.line : t("gui.plugins.disabled_note")),
      el("div", { class: "tags" },
        ...provideChips(p),
        el("span", { class: "chip tag static" }, t(p.host ? "gui.plugins.runs_host" : "gui.plugins.runs_inline")))),
    el("input", {
      type: "checkbox",
      class: "switch",
      checked: p.enabled,
      "aria-label": t("gui.plugins.enabled"),
      onchange: (ev) => {
        if (!ev.target.checked) return pluginCall("/api/plugins/enable", { id: p.id, on: false });
        // Enabling is where it starts receiving: say what first.
        ev.target.checked = false;
        consent(p, {
          title: tf("gui.plugins.enable_title", { name: p.name }),
          intro: t("gui.plugins.enable_intro"),
          label: t("gui.plugins.enable"),
          run: () => pluginCall("/api/plugins/enable", { id: p.id, on: true }),
        }, ev.target);
      },
    }),
    el("button", {
      type: "button",
      class: "ghost",
      onclick: () => {
        // The row folds away, then the plugin goes.
        row.classList.add("leaving");
        setTimeout(() => pluginCall("/api/plugins/remove", { id: p.id }), reducedMotion() ? 0 : 200);
      },
    }, t("gui.remove")));
  return el("div", { class: "plugin-item" }, row, p.fields.length ? pluginSettings(p) : null);
}

async function pluginCall(path, body) {
  pluginError = null;
  try {
    plugins = (await api(path, body)).plugins;
    state.plugins = plugins;
    // A plugin's engine comes and goes with its switch.
    const fresh = await api("/api/state");
    state.engines = fresh.engines;
    state.hooks = fresh.hooks;
  } catch (err) {
    pluginError = err.message;
  }
  pluginViews?.installed(true);
  pluginViews?.market();
  pluginViews?.hooks();
}

function marketCard(p) {
  const have = plugins.some((i) => i.id === p.id);
  const busy = installing === p.id;
  return el("div", { class: "plugin-card" },
    el("div", { class: "top" },
      tileFor(p.id, p.name, p.icon),
      el("div", { class: "svc" }, el("b", { title: p.name }, p.name), el("small", {}, `v${p.version}`)),
      el("button", {
        type: "button",
        class: busy ? "ghost installing" : have ? "ghost" : "main",
        disabled: have || installing !== null,
        onclick: (ev) => confirmInstall(p, ev.currentTarget),
      }, t(have ? "gui.plugins.installed" : busy ? "gui.plugins.installing" : "gui.plugins.install"))),
    el("p", { class: "desc" }, p.description),
    el("div", { class: "tags" },
      p.official ? el("span", { class: "chip tag static official" }, t("gui.market.official")) : null,
      p.provides.includes("engine")
        ? el("span", { class: "chip tag static" }, t(p.streaming ? "gui.engine.streaming" : "gui.engine.batch"))
        : null,
      ...provideChips(p),
      el("span", { class: "chip tag static" }, t(p.host ? "gui.plugins.runs_host" : "gui.plugins.runs_inline")),
      ...p.permissions.map((x) => el("span", { class: "chip tag static" }, x))));
}

async function loadMarket() {
  // A retry keeps what is listed and only says it is trying; the first load
  // shows placeholders.
  marketState = marketState.list.length
    ? { ...marketState, retrying: true }
    : { status: "loading", list: [], reason: "" };
  pluginViews?.market();
  try {
    const market = await api("/api/plugins/market", {});
    marketState = { status: market.unreachable ? "partial" : "ready", list: market.plugins, reason: market.unreachable ?? "" };
  } catch (err) {
    marketState = { status: "failed", list: [], reason: err.message };
  }
  pluginViews?.installed();
  pluginViews?.market();
}

// The hooks a dictation runs, in order, with the built-in pieces that serve
// them. Context and transform hooks move; the order is `[plugins] order`.
function hooksSection() {
  const box = el("div", { class: "group" });
  const switches = el("div", { class: "group" });
  const draw = () => {
    const { run, builtin } = state.hooks;
    const ids = [...new Set(run.filter((h) => h.movable).map((h) => h.plugin))];
    const move = (id, by) => {
      const next = [...ids];
      const at = next.indexOf(id);
      [next[at], next[at + by]] = [next[at + by], next[at]];
      commit((c) => { c.plugins.order = next; }, { field: "plugins.order" }).then(() => { draw(); });
    };
    box.replaceChildren(...(run.length ? run.map((h) => {
      const at = ids.indexOf(h.plugin);
      const arrow = (by, glyph, key) => el("button", {
        type: "button",
        class: "ghost",
        disabled: !h.movable || at + by < 0 || at + by >= ids.length,
        title: tf(key, { name: h.name }),
        "aria-label": tf(key, { name: h.name }),
        onclick: () => move(h.plugin, by),
      }, glyph);
      return el("div", { class: "row hook" },
        el("div", { class: "svc" }, el("b", {}, h.name),
          el("small", {}, t(PROVIDE_NAME[h.hook]), " · ", t(HOOK_RUNS[h.runs]))),
        arrow(-1, "↑", "gui.hooks.up"),
        arrow(1, "↓", "gui.hooks.down"));
    }) : [el("div", { class: "empty-line" }, t("gui.hooks.empty"))]));
    switches.replaceChildren(...builtin.map((b) => {
      const sw = el("input", { type: "checkbox", class: "switch", checked: b.on, "aria-label": tf("gui.hooks.switch", { name: b.name }) });
      sw.addEventListener("change", async () => {
        await commit((c) => { c.plugins.builtin = { ...(c.plugins.builtin ?? {}), [b.id]: sw.checked }; }, {
          revert: () => { sw.checked = b.on; },
        });
        const fresh = await api("/api/state");
        state.hooks = fresh.hooks;
        draw();
      });
      return row(b.name, b.help, sw);
    }), row(t("gui.hooks.replacements_file"), t("gui.hooks.replacements_file_help"), pathLine(state.info.replacements_path), { stack: true }));
  };
  draw();
  return {
    draw,
    nodes: [
      el("section", { class: "section" },
        el("h2", {}, t("gui.hooks.title")),
        box,
        errorSlot("plugins.order"),
        el("p", { class: "foot-note" }, t("gui.hooks.intro"))),
      el("section", { class: "section" },
        el("h2", {}, t("gui.hooks.builtin_group")),
        switches),
    ],
  };
}

function buildPlugins() {
  plugins = state.plugins;
  let query = "";
  let installedKey = null;
  let marketKey = null;
  const installedBox = el("div", { class: "group" });
  const errorLine = el("p", { class: "field-error", role: "alert", hidden: true });
  const marketBox = el("div");
  const search = el("input", { type: "text", spellcheck: "false", placeholder: t("gui.market.search"), "aria-label": t("gui.market.search") });
  search.addEventListener("input", () => { query = search.value.trim().toLowerCase(); drawMarket(); });

  const drawInstalled = (force) => {
    const key = JSON.stringify([plugins, LOCALE, pluginError]);
    setText(errorLine, pluginError ?? "");
    setHidden(errorLine, !pluginError);
    if (key === installedKey && !force) return;
    installedKey = key;
    installedBox.replaceChildren(...(plugins.length
      ? plugins.map(installedItem)
      : [el("div", { class: "empty-line" }, t("gui.plugins.empty"))]));
  };
  let listed = new Set();
  const drawMarket = () => {
    const key = JSON.stringify([marketState, plugins.map((p) => p.id), installing, query, LOCALE]);
    if (key === marketKey) return;
    marketKey = key;
    setHidden(search, marketState.list.length === 0);
    if (marketState.status === "loading") {
      marketBox.replaceChildren(el("div", { class: "market", "aria-busy": "true" },
        ...[0, 1, 2, 3].map(() => el("div", { class: "skeleton" }))));
    } else if (marketState.status === "failed") {
      marketBox.replaceChildren(el("div", { class: "calm" },
        el("b", {}, t("gui.market.unreachable")),
        el("p", { title: marketState.reason }, t("gui.market.unreachable_help")),
        el("button", { type: "button", class: "ghost", onclick: loadMarket }, t("gui.market.retry"))));
    } else {
      // The official plugins come with Anya; only the registry's can be out
      // of reach, which one quiet line under the cards says.
      const partial = marketState.status === "partial" ? el("p", { class: "market-note", role: "status" },
        el("span", { title: marketState.reason }, t("gui.market.partial")),
        el("button", { type: "button", disabled: marketState.retrying, onclick: loadMarket },
          t(marketState.retrying ? "gui.market.retrying" : "gui.market.retry"))) : null;
      const shown = marketState.list.filter((p) => !query
        || `${p.name} ${p.description} ${p.id}`.toLowerCase().includes(query));
      // Plugins the market did not list a moment ago (a retry that reached
      // the registry) fade in; the rest stay as they were.
      const before = listed;
      listed = new Set(marketState.list.map((p) => p.id));
      const cards = shown.length
        ? el("div", { class: "market" }, ...shown.map((p) => {
          const card = marketCard(p);
          if (before.size && !before.has(p.id)) card.classList.add("arrived");
          return card;
        }))
        : el("div", { class: "calm" }, el("p", {}, t(marketState.list.length ? "gui.market.no_match" : "gui.plugins.market_empty")));
      marketBox.replaceChildren(...[cards, partial].filter(Boolean));
    }
  };
  const hooks = hooksSection();
  pluginViews = { installed: drawInstalled, market: drawMarket, hooks: hooks.draw };
  live = (data) => {
    plugins = data.plugins;
    drawInstalled();
    drawMarket();
  };
  drawInstalled();
  drawMarket();
  loadMarket();
  return [
    el("section", { class: "section" },
      el("h2", {}, t("gui.plugins.installed_group")),
      installedBox,
      errorLine),
    ...hooks.nodes,
    el("section", { class: "section" },
      el("h2", {}, t("gui.plugins.market"), el("span", { class: "spacer" }), search),
      marketBox,
      el("p", { class: "foot-note" }, t("gui.plugins.intro"))),
  ];
}

// ---- installing a plugin ----

let pendingInstall = null;

const sheetOpener = {};

function showSheet(id, opener) {
  const backdrop = $(id);
  sheetOpener[id] = opener ?? document.activeElement;
  backdrop.hidden = false;
  requestAnimationFrame(() => backdrop.classList.add("open"));
}

function hideSheet(id, after) {
  const backdrop = $(id);
  backdrop.classList.remove("open");
  setTimeout(() => { if (!backdrop.classList.contains("open")) backdrop.hidden = true; after?.(); }, reducedMotion() ? 0 : 180);
  const opener = sheetOpener[id];
  delete sheetOpener[id];
  if (opener?.isConnected) opener.focus({ preventScroll: true });
}

// Tab stays inside an open sheet.
document.addEventListener("keydown", (ev) => {
  if (ev.key !== "Tab") return;
  const sheet = document.querySelector(".sheet-backdrop.open");
  if (!sheet) return;
  const stops = [...sheet.querySelectorAll("button, input, select, [tabindex]")].filter((n) => !n.disabled && n.offsetParent !== null);
  if (!stops.length) return;
  const first = stops[0];
  const last = stops[stops.length - 1];
  const at = document.activeElement;
  if (!sheet.contains(at) || (ev.shiftKey && at === first)) { ev.preventDefault(); (ev.shiftKey ? last : first).focus(); }
  else if (!ev.shiftKey && at === last) { ev.preventDefault(); first.focus(); }
});

// The consent, for installing and for enabling: what the plugin receives per
// kind it provides, where it runs and what it asks for.
function consent(p, { title, intro, label, run }, opener) {
  pendingInstall = run;
  $("installTitle").textContent = title;
  $("installConfirm").textContent = label;
  $("installBody").replaceChildren(
    el("p", { class: "sheet-sub" }, p.description),
    el("p", { class: "sheet-sub" }, intro),
    el("ul", { class: "perms" }, ...disclosure(p)));
  showSheet("install", opener);
  $("installConfirm").focus();
}

function confirmInstall(p, opener) {
  consent(p, {
    title: tf("gui.plugins.install_title", { name: p.name }),
    intro: t("gui.plugins.install_intro"),
    label: t("gui.plugins.install"),
    run: async () => {
      installing = p.id;
      pluginViews?.market();
      freshPlugin = p.id;
      await pluginCall("/api/plugins/install", { id: p.id });
      freshPlugin = null;
      installing = null;
      pluginViews?.market();
    },
  }, opener);
}

async function confirmed() {
  const run = pendingInstall;
  pendingInstall = null;
  hideSheet("install");
  await run?.();
}

$("installCancel").addEventListener("click", () => { pendingInstall = null; hideSheet("install"); });
$("installConfirm").addEventListener("click", confirmed);
$("install").addEventListener("click", (ev) => { if (ev.target === ev.currentTarget) { pendingInstall = null; hideSheet("install"); } });
$("install").addEventListener("keydown", (ev) => { if (ev.key === "Escape") { ev.preventDefault(); pendingInstall = null; hideSheet("install"); } });

// ---- 5. overlay ----

const SPOTS = [
  ["notch", 50, 5],
  ["top-left", 14, 22],
  ["top-center", 50, 22],
  ["top-right", 86, 22],
  ["bottom-left", 14, 82],
  ["bottom-center", 50, 82],
  ["bottom-right", 86, 82],
];

function buildOverlay() {
  const positions = Object.fromEntries(state.positions.map((p) => [p.value, p.label]));
  const mac = platform() === "macos";
  const screen = el("div", { class: "screen", role: "radiogroup", "aria-label": t("gui.position.label") });
  const drawSpots = () => {
    for (const spot of screen.querySelectorAll(".pos")) spot.remove();
    for (const [value, x, y] of SPOTS) {
      if (value === "notch" && !mac) continue;
      const dot = el("button", {
        type: "button",
        class: value === "notch" ? "pos notch" : "pos",
        role: "radio",
        title: positions[value] ?? value,
        "aria-label": positions[value] ?? value,
        "aria-checked": String(cfg().overlay.position === value),
        onclick: async () => {
          if (cfg().overlay.position === value) return;
          await commit((c) => { c.overlay.position = value; }, { field: "overlay.position" });
          drawSpots();
        },
      });
      // The page's policy refuses a style attribute; the CSSOM is allowed.
      if (value !== "notch") {
        dot.style.left = `${x}%`;
        dot.style.top = `${y}%`;
      }
      screen.append(dot);
    }
  };
  drawSpots();

  let view = "listening";
  const pill = el("button", {
    class: "pill live",
    type: "button",
    onclick: async () => {
      showError("overlay.preview", "");
      try {
        await api("/api/overlay/preview", { view });
      } catch (err) {
        showError("overlay.preview", err.message);
      }
    },
  });
  const drawPill = () => {
    pill.className = `pill live${view === "error" ? " error" : ""}`;
    if (view === "listening") {
      const bars = levelBars();
      bars.setLevel(0.55);
      pill.replaceChildren(bars, el("span", { class: "text" }, t("overlay.sample_text")));
    } else if (view === "done") {
      pill.replaceChildren(el("b", { class: "tone" }, "✓"), el("span", {}, t("overlay.pasted")), el("span", { class: "text hint" }, t("overlay.sample_text")));
    } else {
      pill.replaceChildren(el("b", { class: "tone" }, "!"), el("span", {}, t("error.connect")));
    }
  };
  drawPill();
  const modes = segmented(
    [["listening", t("gui.overlay.preview_listening")], ["done", t("gui.overlay.preview_done")], ["error", t("gui.overlay.preview_error")]],
    view,
    (v) => { view = v; drawPill(); },
    t("gui.overlay.preview"));
  const preview = el("div", { class: "preview" }, pill, modes);

  const percent = (n) => `${n}%`;
  return [
    group(null, [
      row(t("gui.overlay.show"), null, bindToggle("overlay.enabled", t("gui.overlay.show"))),
      row(t("gui.overlay.idle_visible"), null, bindToggle("overlay.idle_visible", t("gui.overlay.idle_visible"))),
      row(t("gui.overlay.show_text"), null, bindToggle("overlay.show_text", t("gui.overlay.show_text")))]),
    group(null, [
      row(t("gui.position.label"), t("gui.position.help"), ctl(screen), { error: "overlay.position", stack: true })]),
    group(t("gui.overlay.group_look"), [
      row(t("gui.overlay.preview"), t("gui.overlay.preview_help"), preview, { error: "overlay.preview", stack: true }),
      row(t("gui.overlay.scale"), null, slider({
        min: 50, max: 200, step: 5, value: Math.round(cfg().overlay.scale * 100), format: percent,
        aria: t("gui.overlay.scale"),
        onchange: (n, back) => commit((c) => { c.overlay.scale = n / 100; }, { field: "overlay.scale", revert: () => back(Math.round(cfg().overlay.scale * 100)) }),
      }), { error: "overlay.scale" })]),
  ];
}

// ---- 6. apps: the subscriber server, the LAN listener, paired clients ----

const SCOPES = [
  ["dictate", "gui.apps.scope_dictate", "gui.apps.scope_dictate_help"],
  ["subscribe_continuous", "gui.apps.scope_continuous", "gui.apps.scope_continuous_help"],
  ["subscribe_dictation", "gui.apps.scope_dictation", "gui.apps.scope_dictation_help"],
];

let lan = { state: "off" };
let clients = [];
let confirming = null; // the id whose Remove awaits a second click
let clientsKey = null;
let clientsRedraw = () => {};

function lastSeenText(c) {
  if (c.connections > 0) {
    return c.connections > 1 ? tf("gui.apps.connected_many", { count: c.connections }) : t("gui.apps.connected");
  }
  if (c.last_seen == null) return t("gui.apps.never");
  return tf("gui.apps.last_seen", { when: ago(c.last_seen) });
}

function clientItem(c) {
  const scopes = SCOPES.map(([scope, label, help]) => el("label", { title: t(help) },
    el("input", {
      type: "checkbox",
      class: "switch",
      checked: c.scopes.includes(scope),
      onchange: (ev) => setScopes(c, scope, ev.target.checked),
    }),
    el("span", {}, t(label))));
  const remove = confirming === c.id
    ? el("span", { class: "confirm" },
      el("span", { class: "muted" }, t("gui.apps.remove_confirm")),
      el("button", { type: "button", class: "danger", onclick: () => removeClient(c) }, t("gui.remove")),
      el("button", { type: "button", class: "ghost", onclick: () => { confirming = null; clientsRedraw(); } }, t("common.cancel")))
    : el("button", { type: "button", class: "ghost", onclick: () => { confirming = c.id; clientsRedraw(); } }, t("gui.remove"));
  return el("div", { class: "item client" },
    el("div", { class: "head" },
      el("b", { class: "name", title: c.name }, c.name),
      el("span", { class: c.connections > 0 ? "state-ok" : "muted" }, lastSeenText(c)),
      el("span", { class: "spacer" }),
      remove),
    el("div", { class: "scopes" }, ...scopes));
}

function setClients(list) {
  clients = list;
  state.clients = list;
  if (confirming && !list.some((c) => c.id === confirming)) confirming = null;
  // Last seen reads in minutes; redrawing on every poll would eat clicks.
  const key = JSON.stringify([list.map((c) => [c.id, c.name, c.scopes, c.connections, c.last_seen == null ? null : Math.floor(c.last_seen / 60000)]), confirming, LOCALE]);
  if (key === clientsKey) return;
  clientsKey = key;
  clientsRedraw();
  noticeNewClient(list);
}

async function setScopes(c, scope, on) {
  const scopes = SCOPES.map(([s]) => s).filter((s) => (s === scope ? on : c.scopes.includes(s)));
  try {
    setClients((await api("/api/clients/scopes", { id: c.id, scopes })).clients);
  } catch (err) {
    notify(err.message, "error");
    clientsKey = null;
    setClients(clients);
  }
}

async function removeClient(c) {
  confirming = null;
  try {
    setClients((await api("/api/clients/remove", { id: c.id })).clients);
  } catch (err) {
    notify(err.message, "error");
    clientsKey = null;
    setClients(clients);
  }
}

function buildApps() {
  const dot = el("i", { class: "dot" });
  const line = el("b");
  const url = el("code", { class: "url" });
  let address = "";
  const copy = copyButton(() => address, "gui.apps.copy_url");
  const card = el("div", { class: "card status-card" }, dot, line, el("span", { class: "spacer" }), copy, url);

  const lanState = el("span");
  let fingerprintText = "";
  const fingerprint = el("code");
  const fingerprintRow = row(t("gui.pairing.fingerprint"), null,
    el("div", { class: "paths" }, fingerprint, copyButton(() => fingerprintText)));
  const addApp = el("button", { type: "button", class: "ghost", onclick: (ev) => openPairing(ev.currentTarget) }, t("gui.apps.add"));
  const addNote = el("p", { class: "note" });
  const clientList = el("div", { class: "group" });
  clientsKey = null;
  clientsRedraw = () => clientList.replaceChildren(...(clients.length
    ? clients.map(clientItem)
    : [el("div", { class: "empty-line" }, t("gui.apps.empty"))]));

  const drawLan = (status) => {
    lan = status;
    setClass(lanState, status.state === "missing" ? "note error" : "note");
    setText(lanState, status.state === "off" ? "" : status.line);
    fingerprintText = status.fingerprint ?? "";
    setText(fingerprint, fingerprintText);
    setAttr(fingerprint, "title", fingerprintText || null);
    setHidden(fingerprintRow, !fingerprintText);
    const off = status.state !== "ok";
    if (addApp.disabled !== off) addApp.disabled = off;
    setText(addNote, status.state === "off" ? t("gui.apps.need_lan") : "");
    setHidden(addNote, status.state !== "off");
  };
  const drawSubscription = (sub) => {
    const ok = sub.state === "ok";
    setClass(dot, `dot ${ok ? "ok" : "error"}`);
    setText(line, sub.line);
    toggleClass(card, "error", !ok);
    address = sub.url ?? "";
    setText(url, address);
    setAttr(url, "title", address || null);
    setHidden(url, !ok);
    setHidden(copy, !ok);
  };
  drawLan(state.lan);
  setClients(state.clients);
  live = (data) => { drawSubscription(data.subscription); drawLan(data.lan); setClients(data.clients); };

  const port = el("input", { type: "number", class: "narrow", min: "1", max: "65535", step: "1", "aria-label": t("gui.lan.port") });
  port.value = cfg().subscription.lan_port;
  port.addEventListener("change", () => commit((c) => { c.subscription.lan_port = Number.parseInt(port.value, 10) || 0; }, { field: "subscription.lan_port" }));

  const addressInput = bindText("subscription.address", { placeholder: state.default_address, aria: t("gui.subscription.label") });
  return [
    card,
    group(t("gui.subscription.label"), [
      row(t("gui.subscription.label"), t("gui.subscription.help"), ctl(addressInput), { error: "subscription.address", top: true })]),
    group(t("gui.apps.receives"), [
      row(t("common.continuous"), null, el("span", { class: "value" }, t("gui.apps.receives_continuous"))),
      row(t("common.dictation"), null, el("span", { class: "value" }, t("gui.apps.receives_dictation")))]),
    group(t("gui.lan.group"), [
      row(t("gui.lan.label"), t("gui.lan.help"), ctl(bindToggle("subscription.lan", t("gui.lan.label"))), { top: true }),
      row(t("gui.lan.port"), t("gui.lan.port_help"), ctl(port), { top: true }),
      fingerprintRow], { note: lanState }),
    el("section", { class: "section" },
      el("h2", {}, t("gui.apps.label"), el("span", { class: "spacer" }), addApp),
      clientList,
      addNote),
  ];
}

let pairing = null; // { deadline, timer, known: ids at the time the code was made }

function stopCountdown() {
  if (pairing?.timer) clearInterval(pairing.timer);
  if (pairing) pairing.timer = null;
}

function tickCountdown() {
  if (!pairing?.timer) return;
  const left = Math.max(0, Math.round((pairing.deadline - performance.now()) / 1000));
  const node = $("pairingCountdown");
  if (left === 0) {
    stopCountdown();
    setClass(node, "note error");
    setText(node, t("gui.pairing.expired"));
    $("pairingCode").classList.add("expired");
    return;
  }
  setClass(node, "note");
  setText(node, tf("gui.pairing.expires", { time: `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}` }));
}

// A client that appeared while the dialog is open is the one just paired.
function noticeNewClient(list) {
  if (!pairing?.timer) return;
  const fresh = list.find((c) => !pairing.known.includes(c.id));
  if (!fresh) return;
  stopCountdown();
  $("pairingCountdown").className = "note";
  $("pairingCountdown").textContent = tf("gui.pairing.paired", { name: fresh.name });
  $("pairingCode").classList.add("expired");
}

async function newPairingCode() {
  stopCountdown();
  $("pairingError").hidden = true;
  try {
    const p = await api("/api/pairing", {});
    $("pairingCode").textContent = p.code;
    $("pairingCode").classList.remove("expired");
    $("pairingUrl").textContent = p.url;
    $("pairingFingerprint").textContent = p.fingerprint;
    const svg = new DOMParser().parseFromString(p.qr, "image/svg+xml").documentElement;
    $("pairingQr").replaceChildren(document.importNode(svg, true));
    pairing = { deadline: performance.now() + p.expires_in * 1000, known: clients.map((c) => c.id), timer: null };
    pairing.timer = setInterval(tickCountdown, 1000);
    tickCountdown();
  } catch (err) {
    $("pairingCode").textContent = "";
    $("pairingQr").replaceChildren();
    $("pairingCountdown").textContent = "";
    $("pairingError").textContent = err.message;
    $("pairingError").hidden = false;
  }
}

function openPairing(opener) {
  showSheet("pairing", opener);
  $("pairingDone").focus();
  newPairingCode();
}

function closePairing() {
  stopCountdown();
  pairing = null;
  hideSheet("pairing");
}

$("pairingNew").addEventListener("click", newPairingCode);
$("pairingDone").addEventListener("click", closePairing);
$("pairing").addEventListener("click", (ev) => { if (ev.target === ev.currentTarget) closePairing(); });
$("pairing").addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") { ev.preventDefault(); closePairing(); }
});

// ---- 7. general ----

// What the update check knows, with the actions that fit it. The status poll
// redraws it; Upgrade blocks until brew ends, so the poll shows "Upgrading".
function updateRow() {
  const upgrade = el("button", { type: "button", class: "main", hidden: true }, t("gui.update.upgrade"));
  const check = el("button", { type: "button", class: "ghost" }, t("gui.update.check_now"));
  const line = el("span", {});
  const node = row("", null, ctl(upgrade, check));
  node.querySelector(".label").replaceChildren(line);
  const draw = (u) => {
    setText(line, u.line);
    setHidden(upgrade, !u.can_upgrade);
  };
  check.addEventListener("click", async () => {
    try { draw((await api("/api/update/check", {})).update); } catch (err) { notify(err.message, "error"); }
  });
  upgrade.addEventListener("click", async () => {
    upgrade.disabled = true;
    check.disabled = true;
    try {
      const done = await api("/api/update/upgrade", {});
      draw(done.update);
      notify(done.message);
    } catch (err) {
      notify(err.message, "error");
    }
    upgrade.disabled = false;
    check.disabled = false;
  });
  live = (data) => draw(data.update);
  return node;
}

function buildGeneral() {
  const path = state.info.config_path;
  const signed = (db) => (db > 0 ? `+${db}` : String(db));
  const groups = [
    group(null, [row(t("gui.ui_language.label"), null, ctl(bindSelect("ui.language", state.ui_languages, t("gui.ui_language.label"))))]),
    group(t("gui.general.group_audio"), [row(t("gui.gain.label"), t("gui.gain.help"), slider({
      min: -20, max: 20, step: 1, value: cfg().audio.gain,
      format: (db) => tf("gui.gain.value", { db: signed(db) }),
      aria: t("gui.gain.label"),
      onchange: (db, back) => commit((c) => { c.audio.gain = db; }, { field: "audio.gain", revert: () => back(cfg().audio.gain) }),
    }))]),
    group(t("gui.general.group_about"), [
      row(t("gui.config.label"), t("gui.config.help"), pathLine(path), { stack: true })]),
    group(t("gui.update.label"), [
      row(t("gui.version.label"), null, el("span", { class: "value" }, tf("gui.version.value", {
        version: state.info.version,
        platform: t(platform() === "macos" ? "gui.platform.macos" : "gui.platform.linux"),
      }))),
      updateRow(),
      row(t("gui.update.check.label"), t("gui.update.check.help"), ctl(bindToggle("updates.check", t("gui.update.check.label"))), { top: true })]),
    group(t("gui.general.group_trouble"), [
      row(t("gui.debug.label"), t("gui.debug.help"), bindToggle("debug.enabled", t("gui.debug.label")))]),
  ];
  if (isApp()) {
    groups.push(group(null, [row(t("gui.quit.label"), null, el("button", {
      type: "button",
      class: "danger",
      onclick: () => api("/api/window", { action: "quit" }).catch((err) => notify(err.message, "error")),
    }, t("gui.quit.button")))]));
  }
  return groups;
}

// ---- the macOS window ----
//
// The desktop window's title bar is hidden; dragging the strip over the
// sidebar and the pane moves the window and a double click zooms it, as a
// title bar does.

for (const id of ["dragSide", "dragMain"]) {
  $(id).addEventListener("mousedown", (ev) => {
    if (ev.button !== 0 || !document.body.classList.contains("mac-app") || !window.ipc) return;
    window.ipc.postMessage("drag");
  });
  $(id).addEventListener("dblclick", () => {
    if (!document.body.classList.contains("mac-app") || !window.ipc) return;
    window.ipc.postMessage("zoom");
  });
}

// ---- start ----

let offline = false;

function onStatus(data) {
  if (offline) {
    offline = false;
    notify("");
  }
  lastStatus = data;
  live?.(data);
}

function onOffline() {
  offline = true;
  notify(t("gui.state.offline"), "error");
}

async function boot() {
  try {
    await loadText();
  } catch (err) {
    // Without the catalog there is only the server's own (translated) word.
    $("loadError").textContent = err.message;
    $("loadError").hidden = false;
    return;
  }
  drawNav();
  try {
    applyState(await api("/api/state"), { render: false });
  } catch (err) {
    $("loadError").textContent = tf("gui.load_failed", { error: err.message });
    $("loadError").hidden = false;
    return;
  }
  try {
    hotkeyWarning = sessionStorage.getItem(FLASH) || null;
    sessionStorage.removeItem(FLASH);
  } catch {}
  let start = location.hash.slice(1);
  if (!PAGES.includes(start)) {
    try { start = localStorage.getItem("anya.page"); } catch {}
  }
  page = null;
  showPage(start);
  drawEngineLine();
  poller(onStatus, onOffline, 250);
}

// The title bar condenses once the page scrolls under it, and the thin
// scrollbar shows only while scrolling.
{
  const pane = $("pane");
  let timer = 0;
  pane.addEventListener("scroll", () => {
    toggleClass($("content"), "scrolled", pane.scrollTop > 6);
    pane.classList.add("scrolling");
    clearTimeout(timer);
    timer = setTimeout(() => pane.classList.remove("scrolling"), 700);
  }, { passive: true });
}

// ⌘1 … ⌘9 jump to a page.
document.addEventListener("keydown", (ev) => {
  if (!(ev.metaKey || ev.ctrlKey) || ev.altKey || !/^[1-9]$/.test(ev.key) || rec) return;
  ev.preventDefault();
  showPage(PAGES[Number(ev.key) - 1]);
});

window.addEventListener("hashchange", () => showPage(location.hash.slice(1)));
// The tray panel asks for a page (the engine's) through the shared storage.
window.addEventListener("storage", (ev) => { if (ev.key === "anya.goto" && ev.newValue) showPage(ev.newValue.split(":")[0]); });

// The tray panel, the TUI or a hand edit can change the config while this
// window is open. Take the saved settings again on focus, unless a field is
// being edited.
window.addEventListener("focus", async () => {
  if (!state || rec) return;
  const typing = document.activeElement?.matches("input[type=text], input[type=password], input[type=number]");
  let fresh;
  try { fresh = await api("/api/state"); } catch { return; }
  await saveChain;
  if (typing || rec || reloadIfLocaleChanged(fresh)) return;
  if (JSON.stringify(fresh) !== JSON.stringify(state)) {
    applyState(fresh);
    drawEngineLine();
  }
});

boot();
