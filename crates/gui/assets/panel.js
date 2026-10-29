"use strict";

// The tray panel: live status, the latest dictation, usage totals, switches
// for dictation and continuous transcription, and a read-only summary of the
// other saved settings. Editing happens in the settings window, which
// "Open settings" brings forward.

let state = null;
// The continuous switch shows the running state, which its hotkey can flip
// at any time, so every poll updates it unless a click is still in flight.
let continuousOn = null;
let continuousBusy = false;

function modeLabel(mode) {
  switch (mode) {
    case "toggle": return t("gui.mode.toggle");
    case "hold": return t("gui.mode.hold");
    default: return mode;
  }
}

function outputLabel(output) {
  switch (output) {
    case "paste": return t("gui.panel.output_paste");
    case "clipboard": return t("gui.panel.output_clipboard");
    case "none": return t("gui.panel.output_none");
    default: return output;
  }
}

function summaryRow(label, ...value) {
  return el("div", { class: "row" }, el("span", { class: "label" }, label), el("span", { class: "value" }, ...value));
}

function renderSummary() {
  const c = state.config;
  const rows = [];
  const dictation = el("input", {
    type: "checkbox", class: "switch", "aria-label": t("common.dictation"),
    onchange: () => setSwitch(dictation, "/api/dictation"),
  });
  dictation.checked = c.dictation.enabled;
  rows.push(el("label", { class: "row" }, el("span", { class: "label" }, t("common.dictation")), el("span", { class: "value" }, dictation)));

  const cont = el("input", {
    type: "checkbox", class: "switch", id: "continuousSwitch", "aria-label": t("common.continuous"),
    onchange: () => setContinuous(cont),
  });
  cont.checked = continuousOn ?? c.continuous.enabled;
  rows.push(el("label", { class: "row" }, el("span", { class: "label" }, t("common.continuous")), el("span", { class: "value" }, cont)));

  const hotkeys = splitHotkeys(c.dictation.hotkeys);
  rows.push(summaryRow(t("gui.hotkey.label"), ...(hotkeys.length
    ? hotkeys.flatMap((h, i) => [i ? el("span", { class: "muted" }, t("gui.panel.or")) : null, keycaps(h.split("+"), state.info.platform)])
    : [el("span", { class: "muted" }, t("gui.hotkey.unset"))])));
  rows.push(summaryRow(t("gui.mode.label"), modeLabel(c.dictation.mode)));

  const engine = state.engines.find((e) => e.id === c.recognition.engine);
  rows.push(summaryRow(t("gui.engine.label"), engine ? engine.title : c.recognition.engine,
    !engine || !engine.ready ? el("span", { class: "badge warn" }, t("gui.engine.missing")) : null));
  const lang = state.languages.find((l) => l.value === c.recognition.language);
  rows.push(summaryRow(t("gui.language.label"), lang ? lang.label : c.recognition.language));
  rows.push(summaryRow(t("gui.panel.output"), outputLabel(c.dictation.output)));
  $("summary").replaceChildren(...rows);
}

async function setSwitch(toggle, path) {
  toggle.disabled = true;
  try {
    state = (await api(path, { on: toggle.checked })).state;
  } catch {
    toggle.checked = !toggle.checked;
  }
  renderSummary();
}

async function setContinuous(toggle) {
  toggle.disabled = continuousBusy = true;
  try {
    state = (await api("/api/continuous", { on: toggle.checked })).state;
    continuousOn = toggle.checked;
  } catch {
    toggle.checked = !toggle.checked;
  }
  continuousBusy = false;
  renderSummary();
}

let lastKey = null;

// The dictation being recorded, else the latest dictated text.
function renderLast(data) {
  const s = data.status;
  const busy = s.dictation.state !== "idle" || s.finishing > 0;
  const live = busy && s.live_text ? s.live_text : null;
  let final = null;
  for (let i = data.activity.length - 1; i >= 0 && final === null; i--) {
    const e = data.activity[i];
    if (e.kind === "final" && e.source === "dictation") final = e.text;
  }
  const key = JSON.stringify([live, final, LOCALE]);
  if (key === lastKey) return;
  lastKey = key;
  const box = $("lastText");
  box.classList.toggle("live", live !== null);
  if (live !== null) box.replaceChildren(el("span", {}, tf("gui.live", { text: live })));
  else if (final !== null) box.replaceChildren(el("span", {}, final), copyButton(final));
  else box.replaceChildren(el("span", { class: "muted" }, t("gui.panel.empty")));
}

function renderStatus(data) {
  renderPill(data);
  renderStats(data.stats);
  const cont = $("continuousSwitch");
  if (cont && !continuousBusy && data.status.continuous_on !== continuousOn) {
    continuousOn = data.status.continuous_on;
    cont.checked = continuousOn;
  }
  renderLast(data);
}

async function refreshState() {
  try {
    const fresh = await api("/api/state");
    if (reloadIfLocaleChanged(fresh)) return;
    state = fresh;
    // Only the desktop app has a window to open and an app to quit.
    $("panelFoot").hidden = state.shell !== "app";
    renderSummary();
  } catch {}
}

async function poll() {
  if (!document.hidden) {
    try {
      renderStatus(await api("/api/status"));
    } catch {
      renderOffline();
    }
  }
  setTimeout(poll, 300);
}

async function boot() {
  try {
    await loadText();
  } catch (err) {
    $("lastText").replaceChildren(el("span", { class: "muted" }, err.message));
    return;
  }
  await refreshState();
  poll();
}

// Settings saved in the window show up the next time the panel opens.
window.addEventListener("focus", refreshState);
document.addEventListener("visibilitychange", () => { if (!document.hidden) refreshState(); });
$("settings").addEventListener("click", () => api("/api/window", { action: "settings" }).catch(() => {}));
$("quit").addEventListener("click", () => api("/api/window", { action: "quit" }).catch(() => {}));

boot();
