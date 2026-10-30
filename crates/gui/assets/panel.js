"use strict";

// The tray panel: what Anya is doing, the latest sentence, switches for
// dictation and continuous transcription, and the last few log lines.
// Everything else is edited in the settings window, which "Settings…" brings
// forward.

let state = null;
let data = null;
// A switch shows the running state, which a hotkey can flip at any time, so
// every poll updates it unless a click is still in flight.
let busy = false;
let lastKey = {};

const DOT = {
  idle: "ok", recording: "rec", stopping: "rec", finishing: "rec",
  failed: "error", offline: "error", not_ready: "warn", config_broken: "warn", off: "",
};

function once(slot, key, draw) {
  const text = JSON.stringify([key, LOCALE]);
  if (lastKey[slot] === text) return;
  lastKey[slot] = text;
  draw();
}

function firstHotkey() {
  const list = splitHotkeys(state.config.dictation.hotkeys);
  return list.length ? comboLabel(list[0], state.info.platform) : t("gui.hotkey.unset");
}

// Every dictation hotkey, as the settings page prints them.
function allHotkeys() {
  const list = splitHotkeys(state.config.dictation.hotkeys);
  return list.length ? list.map((h) => comboLabel(h, state.info.platform)).join(" / ") : t("gui.hotkey.unset");
}

function lastSentence() {
  for (let i = data.activity.length - 1; i >= 0; i--) {
    const e = data.activity[i];
    if (e.kind === "final" && e.source === "dictation") return e;
  }
  return null;
}

function card(kind, ...children) {
  return el("div", { class: `top-card ${kind}` }, ...children);
}

function drawTop() {
  const s = data.status;
  const pill = data.pill.state;
  const hold = state.config.dictation.mode === "hold";
  let key;
  let build;
  if (pill === "failed" && data.failure) {
    key = ["failed", data.failure];
    build = () => card("failed",
      el("div", { class: "title error" }, data.failure.text),
      data.failure.detail ? el("p", { class: "detail mono" }, data.failure.detail) : null,
      el("div", { class: "actions" },
        el("button", { type: "button", class: "main", onclick: () => api("/api/dictation/retry", {}).catch(() => {}) }, t("gui.panel.retry")),
        el("button", { type: "button", class: "ghost", onclick: () => api("/api/dictation/dismiss", {}).catch(() => {}) }, t("gui.close"))));
  } else if (pill === "recording" || pill === "stopping" || pill === "finishing") {
    key = ["live", s.live_text, hold, firstHotkey(), pill];
    build = () => {
      const bars = levelBars();
      bars.id = "bars";
      return card("recording",
        el("div", { class: "live-line" }, bars,
          s.live_text ? el("p", {}, s.live_text) : el("p", { class: "muted" }, t("overlay.listening"))),
        pill === "recording"
          ? el("p", { class: "fine" }, hold ? t("gui.panel.stop_hint_hold") : tf("gui.panel.stop_hint_toggle", { hotkey: firstHotkey() }))
          : null);
    };
  } else if (pill === "not_ready") {
    key = ["not_ready"];
    build = () => card("warn",
      el("p", { class: "sentence" }, t("gui.panel.not_ready")),
      el("div", { class: "actions" },
        el("button", { type: "button", class: "main", onclick: openEngine }, t("gui.notready.go"))));
  } else if (pill === "config_broken") {
    const notice = data.activity.slice().reverse().find((e) => e.kind === "notice" && e.level === "warn");
    key = ["config_broken", notice?.text];
    build = () => card("warn", el("p", { class: "sentence" }, notice?.text ?? t("gui.state.config_broken")));
  } else {
    const last = lastSentence();
    key = ["idle", last, data.stats.last_audio_ms];
    build = () => {
      if (!last) return card("", el("p", { class: "muted" }, t("gui.panel.empty")));
      const chars = [...last.text].length;
      // The length of the audio is known only for a sentence heard since Anya started.
      const fine = data.stats.last_audio_ms > 0
        ? tf("gui.panel.last_stats", { chars, seconds: (data.stats.last_audio_ms / 1000).toFixed(1) })
        : tf("gui.panel.last_chars", { chars });
      return card("",
        el("div", { class: "meta" },
          el("span", {}, tf("gui.panel.last", { when: ago(last.at), where: last.note })),
          el("span", { class: "spacer" }),
          copyButton(last.text)),
        el("p", { class: "sentence" }, last.text),
        el("p", { class: "fine" }, fine));
    };
  }
  // The level changes every poll; the card around it only when it must.
  once("top", key, () => $("top").replaceChildren(build()));
  const bars = $("bars");
  if (bars) bars.setLevel(s.level);
}

function openEngine() {
  try { localStorage.setItem("anya.goto", `engine:${Date.now()}`); } catch {}
  api("/api/window", { action: "settings" }).catch(() => {});
}

function outputWord(output) {
  switch (output) {
    case "paste": return t("gui.output.paste");
    case "clipboard": return t("gui.output.clipboard");
    default: return t("gui.output.none");
  }
}

const MIC_ICON = ["M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z", "M5 11a7 7 0 0 0 14 0", "M12 18v3"];
const WAVE_ICON = ["M4 10v4", "M8 6v12", "M12 3v18", "M16 7v10", "M20 10v4"];
const CLOCK_ICON = ["M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z", "M12 7v5l3 2"];
const GEAR_ICON = ["M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z", "M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"];
const POWER_ICON = ["M12 3v9", "M6.3 6.3a8 8 0 1 0 11.4 0"];

// A Control Center toggle: a round icon, filled with the accent colour when
// on, beside its name and what it is set to. The whole row is the control.
function toggleRow(label, icon, sub, path) {
  const button = el("button", {
    type: "button", class: "cc-toggle", role: "switch", "aria-checked": "false", "aria-label": label,
    onclick: () => flip(button, path),
  }, el("span", { class: "cc-icon" }, glyph(icon)), el("span", { class: "cc-text" }, el("b", {}, label), sub));
  return button;
}

function setOn(button, on) {
  setAttr(button, "aria-checked", String(on));
}

// The two toggles are made once; a poll only brings their words and state up
// to date, so a toggle being pressed is never replaced under the finger.
let sw = null;

function buildSwitches() {
  const dSub = el("small");
  const cSub = el("small");
  sw = {
    dSub, cSub,
    dictation: toggleRow(t("common.dictation"), MIC_ICON, dSub, "/api/dictation"),
    cont: toggleRow(t("common.continuous"), WAVE_ICON, cSub, "/api/continuous"),
  };
  $("switches").replaceChildren(sw.dictation, sw.cont);
}

function drawSwitches() {
  if (!sw) buildSwitches();
  const c = state.config;
  const ok = data.engine.ready;
  const mode = t(c.dictation.mode === "hold" ? "gui.mode.hold" : "gui.mode.toggle");
  const source = /^default$/i.test(c.continuous.device) || !c.continuous.device
    ? t("gui.continuous.source_system")
    : (state.devices.find((d) => d.id === c.continuous.device || d.name === c.continuous.device)?.name ?? c.continuous.device);
  const count = data.subscription.subscribers ?? 0;
  toggleClass($("switches"), "disabled", !ok);
  for (const input of [sw.dictation, sw.cont]) if (input.disabled !== !ok) input.disabled = !ok;
  setOn(sw.dictation, c.dictation.enabled);
  setOn(sw.cont, data.status.continuous_on);
  setText(sw.dSub, `${allHotkeys()} · ${mode} · ${outputWord(c.dictation.output)}`);
  setText(sw.cSub, tf("gui.panel.continuous_sub", { source, count }));
}

// The toggle changes at once; a refusal puts it back.
async function flip(button, path) {
  const on = button.getAttribute("aria-checked") !== "true";
  setOn(button, on);
  busy = true;
  try {
    state = (await api(path, { on })).state;
  } catch (err) {
    setOn(button, !on);
    toast(err.message, "error");
  }
  busy = false;
}

// A sentence in the list copies when clicked, and says so in place of its time.
function recentRow(e) {
  const time = el("time", {}, clock(e.at));
  if (e.kind !== "final") {
    return el("li", { class: e.level }, time, el("span", { class: "text", title: e.text }, e.text));
  }
  const row = el("li", { class: "ok" }, el("button", {
    type: "button",
    title: t("gui.copy"),
    onclick: async () => {
      let ok = true;
      try { await api("/api/copy", { text: e.text }); } catch { ok = false; }
      time.textContent = t(ok ? "gui.copied" : "gui.copy_failed");
      row.classList.toggle("copied", ok);
      setTimeout(() => { time.textContent = clock(e.at); row.classList.remove("copied"); }, 1200);
    },
  }, time, el("span", { class: "text" }, e.text)));
  return row;
}

function drawRecent() {
  // The sentence the card above shows is not listed again.
  const shown = data.pill.state === "idle" ? lastSentence() : null;
  const list = data.activity.filter((e) => e.kind !== "debug" && e !== shown).slice(-3).reverse();
  once("recent", [list], () => {
    setHidden($("recent"), !list.length);
    $("recent").replaceChildren(...list.map(recentRow));
  });
}

function drawTotals() {
  const n = (v) => v.toLocaleString(LOCALE);
  const s = data.stats;
  setText($("totals"), tf("gui.panel.totals", { sessions: n(s.sessions), chars: n(s.chars), minutes: n(minutes(s.audio_ms)) }));
}

function render(fresh) {
  data = fresh;
  if (!state) return;
  setAttr($("status"), "title", data.detail ? `${data.pill.label}  ${data.detail}` : data.pill.label);
  setClass($("statusDot"), `dot ${DOT[data.pill.state] ?? ""}`);
  setText($("statusLabel"), data.pill.label);
  drawTop();
  if (!busy) drawSwitches();
  drawRecent();
  drawTotals();
}

function offline() {
  setClass($("statusDot"), "dot error");
  setText($("statusLabel"), t("gui.state.offline"));
}

async function refreshState() {
  try {
    const fresh = await api("/api/state");
    if (reloadIfLocaleChanged(fresh)) return;
    state = fresh;
    // Only the desktop app has a window to open and an app to quit.
    $("panelFoot").hidden = state.shell !== "app";
    lastKey = {};
    sw = null;
    if (data) render(data);
  } catch {}
}

async function boot() {
  try {
    await loadText();
  } catch (err) {
    $("top").replaceChildren(el("p", { class: "muted" }, err.message));
    return;
  }
  await refreshState();
  poller(render, offline, 300);
}

// Settings saved in the window show up the next time the panel opens.
window.addEventListener("focus", refreshState);
// Like a menu, nothing is highlighted until the pointer moves over the panel.
document.addEventListener("mousemove", () => document.body.classList.add("pointer"));
window.addEventListener("blur", () => document.body.classList.remove("pointer"));
document.addEventListener("visibilitychange", () => { if (!document.hidden) refreshState(); });
$("history").append(glyph(CLOCK_ICON));
$("settings").append(glyph(GEAR_ICON));
$("quit").append(glyph(POWER_ICON));
$("history").addEventListener("click", () => {
  try { localStorage.setItem("anya.goto", `activity:${Date.now()}`); } catch {}
  api("/api/window", { action: "settings" }).catch(() => {});
});
$("settings").addEventListener("click", () => api("/api/window", { action: "settings" }).catch(() => {}));
$("quit").addEventListener("click", () => api("/api/window", { action: "quit" }).catch(() => {}));

// The desktop app sizes the panel's window to the page, so nothing is left blank.
if (window.ipc) {
  let posted = 0;
  new ResizeObserver(() => {
    const height = Math.ceil($("wrap").getBoundingClientRect().height);
    if (height !== posted) { posted = height; window.ipc.postMessage(`height:${height}`); }
  }).observe($("wrap"));
}

boot();
