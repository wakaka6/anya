"use strict";

// Helpers shared by the settings window (app.js) and the tray panel (panel.js).
//
// Every word on both pages comes from Anya's catalog (GET /api/i18n). HTML
// names keys in data-i18n attributes; scripts call t("…") with the whole key
// written out, never assembled, so a test can find each one in the catalog.

const $ = (id) => document.getElementById(id);

let LOCALE = "en";
let TEXT = {};
let TEXT_READY = false;

function t(key) {
  return TEXT[key] ?? key;
}

function tf(key, args) {
  return t(key).replace(/\{(\w+)\}/g, (whole, name) => (name in args ? String(args[name]) : whole));
}

function applyText(root = document) {
  for (const node of root.querySelectorAll("[data-i18n]")) node.textContent = t(node.dataset.i18n);
  for (const node of root.querySelectorAll("[data-i18n-placeholder]")) node.placeholder = t(node.dataset.i18nPlaceholder);
  for (const node of root.querySelectorAll("[data-i18n-title]")) node.title = t(node.dataset.i18nTitle);
  for (const node of root.querySelectorAll("[data-i18n-aria]")) node.setAttribute("aria-label", t(node.dataset.i18nAria));
}

async function loadText() {
  const data = await api("/api/i18n");
  LOCALE = data.locale;
  TEXT = data.text;
  TEXT_READY = true;
  document.documentElement.lang = LOCALE;
  applyText();
}

// The page speaks the catalog it loaded. When [ui] language changes (saved
// here, in the TUI or by hand) it loads again in the new language.
function reloadIfLocaleChanged(state) {
  if (!TEXT_READY || state.locale === LOCALE) return false;
  location.reload();
  return true;
}

async function api(path, body) {
  const res = await fetch(path, body === undefined ? {} : {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  let data = {};
  if (res.status !== 204) {
    const type = res.headers.get("Content-Type") || "";
    // Refusals before the API (an expired key) are plain text.
    data = type.startsWith("application/json") ? await res.json() : { error: (await res.text()).trim() };
  }
  if (!res.ok) {
    const err = new Error(data.error || tf("gui.api.failed_status", { status: res.status }));
    err.data = data;
    err.status = res.status;
    throw err;
  }
  return data;
}

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") node.className = v;
    else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
    else if (v !== false && v != null) node.setAttribute(k, v === true ? "" : v);
  }
  node.append(...children.filter((c) => c != null));
  return node;
}

// Names a hotkey may spell a key with, to the name the labels use.
const KEY_NAMES = {
  ctrl: "Ctrl", control: "Ctrl", alt: "Alt", option: "Alt", shift: "Shift",
  super: "Super", cmd: "Super", command: "Super", win: "Super", meta: "Super",
  fn: "Fn", function: "Fn", up: "Up", down: "Down", left: "Left", right: "Right",
  tab: "Tab", capslock: "CapsLock", enter: "Enter", return: "Enter",
  backspace: "Backspace", esc: "Escape", escape: "Escape", menu: "Menu", space: "Space",
};

function keyLabel(name, platform) {
  const mac = platform === "macos";
  switch (KEY_NAMES[name.trim().toLowerCase()]) {
    case "Ctrl": return t(mac ? "gui.key.ctrl_mac" : "gui.key.ctrl");
    case "Alt": return t(mac ? "gui.key.alt_mac" : "gui.key.alt");
    case "Shift": return t(mac ? "gui.key.shift_mac" : "gui.key.shift");
    case "Super": return t(mac ? "gui.key.super_mac" : "gui.key.super");
    case "Fn": return t("gui.key.fn");
    case "Up": return t("gui.key.up");
    case "Down": return t("gui.key.down");
    case "Left": return t("gui.key.left");
    case "Right": return t("gui.key.right");
    case "Tab": return t("gui.key.tab");
    case "CapsLock": return t("gui.key.capslock");
    case "Enter": return t("gui.key.enter");
    case "Backspace": return t("gui.key.backspace");
    case "Escape": return t("gui.key.escape");
    case "Menu": return t("gui.key.menu");
    case "Space": return t("gui.key.space");
    default: return name.trim();
  }
}

// A combination as the keys are printed: symbols side by side on macOS,
// joined with "+" elsewhere.
function comboLabel(combo, platform) {
  return combo.split("+").map((k) => keyLabel(k, platform)).join(platform === "macos" ? " " : "+");
}

// Splits a hotkey list like Anya's parser: a comma right after "+" is the key.
function splitHotkeys(s) {
  const out = [];
  let cur = "";
  for (const ch of s || "") {
    if ((ch === "," || ch === "，") && !cur.trimEnd().endsWith("+")) {
      if (cur.trim()) out.push(cur.trim());
      cur = "";
    } else {
      cur += ch;
    }
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}

const COPY_ICON = ["M9 9h10v10H9z", "M5 15V5h10"];
const CHECK_ICON = ["M5 12.5l4.5 4.5L19 7.5"];

// A stroked glyph as an inline SVG.
function glyph(paths) {
  const svg = new DOMParser().parseFromString(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">${paths.map((d) => `<path d="${d}"/>`).join("")}</svg>`,
    "image/svg+xml").documentElement;
  return document.importNode(svg, true);
}

// Copies through Anya, which reaches the system clipboard from the desktop
// window and a browser alike. An icon button: its tooltip names the action
// and says how it went for a moment.
function copyButton(text, label = "gui.copy") {
  const button = el("button", { type: "button", class: "icon-btn", title: t(label), "aria-label": t(label) }, glyph(COPY_ICON));
  button.addEventListener("click", async () => {
    let ok = true;
    try {
      await api("/api/copy", { text: typeof text === "function" ? text() : text });
    } catch {
      ok = false;
    }
    const said = t(ok ? "gui.copied" : "gui.copy_failed");
    button.title = said;
    button.setAttribute("aria-label", said);
    button.classList.toggle("done", ok);
    button.classList.toggle("failed", !ok);
    button.replaceChildren(glyph(ok ? CHECK_ICON : COPY_ICON));
    setTimeout(() => {
      button.title = t(label);
      button.setAttribute("aria-label", t(label));
      button.classList.remove("done", "failed");
      button.replaceChildren(glyph(COPY_ICON));
    }, 1200);
  });
  return button;
}

// How long ago `ms` was, in words.
function ago(ms) {
  const minutes = Math.floor((Date.now() - ms) / 60000);
  if (minutes < 1) return t("gui.time.just_now");
  if (minutes < 60) return tf("gui.time.minutes_ago", { count: minutes });
  if (minutes < 1440) return tf("gui.time.hours_ago", { count: Math.floor(minutes / 60) });
  return new Date(ms).toLocaleDateString(LOCALE);
}

function clock(ms) {
  return new Date(ms).toLocaleTimeString(LOCALE, { hour: "2-digit", minute: "2-digit", hour12: false });
}

// A time of day to the millisecond, for the lines debug mode records.
function clockMs(ms) {
  const d = new Date(ms);
  const pad = (n, w = 2) => String(n).padStart(w, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

// Whole minutes of recorded audio, at least one once there is any.
function minutes(ms) {
  return ms > 0 ? Math.max(1, Math.round(ms / 60000)) : 0;
}

// Writers that touch the DOM only when the value changed: a poll that finds
// nothing new must leave the page alone (selection, hover and layout stay).
function setText(node, value) {
  const v = String(value ?? "");
  if (node.textContent !== v) node.textContent = v;
}

function setAttr(node, name, value) {
  if (value == null || value === false) {
    if (node.hasAttribute(name)) node.removeAttribute(name);
  } else {
    const v = value === true ? "" : String(value);
    if (node.getAttribute(name) !== v) node.setAttribute(name, v);
  }
}

function setHidden(node, hidden) {
  if (node.hidden !== !!hidden) node.hidden = !!hidden;
}

function setClass(node, name) {
  if (node.className !== name) node.className = name;
}

function toggleClass(node, name, on) {
  if (node.classList.contains(name) !== !!on) node.classList.toggle(name, !!on);
}

const reducedMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

// ---- the floating message ----
//
// A save is confirmed by a small check that fades in over the content and out
// again; nothing moves. An error stays until dismissed or the next action.

let toastNode = null;
let toastTimer = 0;

function toastBox() {
  if (!toastNode) {
    toastNode = el("div", { id: "toast", class: "toast", role: "status" });
    document.body.append(toastNode);
  }
  return toastNode;
}

function toast(text, kind = "ok") {
  const box = toastBox();
  clearTimeout(toastTimer);
  box.replaceChildren(el("span", { class: "mark" }, kind === "ok" ? "✓" : "!"), el("span", { class: "msg" }, text));
  box.title = text;
  if (kind === "error") {
    box.append(el("button", { type: "button", class: "x", "aria-label": t("gui.close"), onclick: dismissToast }, "×"));
  }
  box.className = `toast ${kind}`;
  void box.offsetWidth;
  box.classList.add("show");
  if (kind === "ok") toastTimer = setTimeout(dismissToast, 900);
}

function dismissToast() {
  clearTimeout(toastTimer);
  toastNode?.classList.remove("show");
}

function toastIsError() {
  return !!toastNode?.classList.contains("show") && toastNode.classList.contains("error");
}

// The webview's own menu only where text can be selected or edited.
document.addEventListener("contextmenu", (ev) => {
  if (!ev.target.closest?.("input, textarea, code, .selectable, .log .text, .log .detail")) ev.preventDefault();
});

const BAR_WEIGHTS = [0.45, 0.75, 1, 0.7, 0.9, 0.55, 0.8];

// A row of bars that follow an input level from 0 to 1.
function levelBars() {
  const bars = BAR_WEIGHTS.map(() => el("i"));
  const node = el("span", { class: "bars", "aria-hidden": "true" }, ...bars);
  let last = null;
  node.setLevel = (level) => {
    const shaped = Math.min(1, Math.max(0, level ?? 0) ** 0.6 * 1.4);
    const key = Math.round(shaped * 100);
    if (key === last) return;
    last = key;
    bars.forEach((bar, i) => { bar.style.height = `${Math.round(18 + 82 * shaped * BAR_WEIGHTS[i])}%`; });
  };
  node.setLevel(0);
  return node;
}

// A tray panel or settings window poll of the live state.
function poller(render, onOffline, every) {
  const tick = async () => {
    if (!document.hidden) {
      try {
        render(await api("/api/status"));
      } catch {
        onOffline();
      }
    }
    setTimeout(tick, every);
  };
  tick();
}
