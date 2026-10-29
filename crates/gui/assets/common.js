"use strict";

// Helpers shared by the settings page (app.js) and the tray panel (panel.js).
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

function keycaps(names, platform) {
  return el("span", { class: "keycaps" }, ...names.map((n) => el("kbd", {}, keyLabel(n, platform))));
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

// Characters per minute of recorded audio.
function cpm(chars, ms) {
  return ms > 0 ? Math.round(chars / (ms / 60000)) : 0;
}

// Copies through Anya, which reaches the system clipboard from the desktop
// window and a browser alike.
function copyButton(text) {
  const button = el("button", {
    type: "button",
    class: "ghost",
    onclick: async () => {
      let ok = true;
      try {
        await api("/api/copy", { text });
      } catch {
        ok = false;
      }
      button.textContent = t(ok ? "gui.copied" : "gui.copy_failed");
      setTimeout(() => { button.textContent = t("gui.copy"); }, 1200);
    },
  }, t("gui.copy"));
  return button;
}

function renderPill(data) {
  $("status").dataset.state = data.pill.state;
  $("statusLabel").textContent = data.pill.label;
  if ($("statusDetail")) $("statusDetail").textContent = data.detail;
  $("status").title = data.detail ? `${data.pill.label}  ${data.detail}` : data.pill.label;
}

function renderOffline() {
  $("status").dataset.state = "offline";
  $("statusLabel").textContent = t("gui.state.offline");
  if ($("statusDetail")) $("statusDetail").textContent = "";
  $("status").title = t("gui.state.offline");
}

function renderStats(stats) {
  const n = (v) => v.toLocaleString(LOCALE);
  $("statSessions").textContent = n(stats.sessions);
  $("statChars").textContent = n(stats.chars);
  $("statAvg").textContent = n(cpm(stats.chars, stats.audio_ms));
  $("statLast").textContent = n(cpm(stats.last_chars, stats.last_audio_ms));
}
