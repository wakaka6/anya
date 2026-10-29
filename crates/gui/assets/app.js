"use strict";

// One page for the desktop window and `anya --web`. `saved` is the last state
// the server returned; `draft` is the config being edited, engine settings
// included (`draft.recognition.engines`); `ready` says per engine whether its
// draft settings are enough to open a session.

const form = $("form");

let saved = null;
let draft = null;
let ready = {};
let saving = false;

const clone = (v) => JSON.parse(JSON.stringify(v));

// JSON with sorted keys, so two configs compare equal whatever order a map
// came back in.
function canonical(v) {
  return JSON.stringify(v, (k, val) => (val && typeof val === "object" && !Array.isArray(val)
    ? Object.fromEntries(Object.keys(val).sort().map((key) => [key, val[key]]))
    : val));
}

function getPath(obj, path) {
  return path.split(".").reduce((o, k) => o?.[k], obj);
}

function setPath(obj, path, value) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((o, k) => o[k], obj)[last] = value;
}

// ---- tabs ----

const PANES = ["general", "engine", "activity"];

function showPane(name) {
  if (!PANES.includes(name)) name = PANES[0];
  for (const p of PANES) {
    $(`pane-${p}`).hidden = p !== name;
    const tab = $(`tab-${p}`);
    tab.setAttribute("aria-selected", String(p === name));
    tab.tabIndex = p === name ? 0 : -1;
  }
  try { localStorage.setItem("anya.pane", name); } catch {}
}

for (const tab of document.querySelectorAll('[role="tab"]')) {
  tab.addEventListener("click", () => showPane(tab.dataset.pane));
  tab.addEventListener("keydown", (ev) => {
    const step = { ArrowRight: 1, ArrowLeft: -1 }[ev.key];
    if (!step) return;
    const next = PANES[(PANES.indexOf(tab.dataset.pane) + step + PANES.length) % PANES.length];
    showPane(next);
    $(`tab-${next}`).focus();
  });
}

// ---- the form ----
//
// A control with data-path edits that config key; errors for a key show in
// the elements whose data-error names it.

function readControl(node) {
  if (node.type === "checkbox") return node.checked;
  if (node.type === "number") return Math.min(Math.max(Number.parseInt(node.value, 10) || 0, 0), 4294967295);
  return node.value;
}

function writeControl(node, value) {
  if (node.type === "checkbox") node.checked = !!value;
  else node.value = value ?? "";
}

function fillOptions(select, options) {
  select.replaceChildren(...options.map((o) => el("option", { value: o.value }, o.label)));
}

function outputOptions() {
  return [
    { value: "paste", label: t("gui.output.paste") },
    { value: "clipboard", label: t("gui.output.clipboard") },
    { value: "none", label: t("gui.output.none") },
  ];
}

function applyState(state) {
  saved = state;
  draft = clone(state.config);
  ready = Object.fromEntries(state.engines.map((e) => [e.id, e.ready]));
  document.body.classList.toggle("mac-app", state.shell === "app" && state.info.platform === "macos");
  $("configPath").textContent = state.info.config_path;
  $("configPath").title = state.info.config_path;
  $("address").placeholder = state.default_address;
  fillOptions($("output"), outputOptions());
  fillOptions($("language"), state.languages);
  fillOptions($("position"), state.positions);
  fillOptions($("uiLanguage"), state.ui_languages);
  for (const node of form.querySelectorAll("[data-path]")) writeControl(node, getPath(draft, node.dataset.path));
  endRecording();
  clearErrors();
  renderSlots();
  renderDevices();
  renderHotwords();
  renderHotwordsFile();
  renderModes();
  renderEngines();
  updateDirty();
}

function errorNodes(path) {
  return document.querySelectorAll(`[data-error="${path}"]`);
}

function showError(path, text) {
  for (const node of errorNodes(path)) {
    node.textContent = text;
    node.hidden = !text;
  }
}

function clearErrors() {
  for (const node of document.querySelectorAll("[data-error]")) {
    node.textContent = "";
    node.hidden = true;
  }
  for (const node of document.querySelectorAll(".invalid")) node.classList.remove("invalid");
}

// ---- hotkey slots ----
//
// Two groups: the dictation hotkeys and the one that switches continuous
// transcription. Each is a comma-separated list whose first entries are the
// slots shown here; further ones (a hand-edited config) are kept as they are.
// In the desktop app a slot is recorded by the OS, which sees keys a webview
// cannot (Fn); in a browser, or when the OS recorder is unavailable, from
// this page's key events while the global hotkeys are paused.

const HOTKEY_GROUPS = {
  "dictation.hotkeys": { container: "slots", names: () => [t("gui.hotkey.primary"), t("gui.hotkey.secondary")], extra: "hotkeyExtra" },
  "continuous.hotkey": { container: "continuousSlots", names: () => [t("gui.continuous.slot")], extra: null },
};
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

let rec = null; // { group, slot, mode: "os" | "keys", mods: Set, ready, keepalive }

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

function renderSlots() {
  if (!draft) return;
  for (const [path, group] of Object.entries(HOTKEY_GROUPS)) renderGroup(path, group);
}

function renderGroup(path, group) {
  const list = splitHotkeys(getPath(draft, path));
  const names = group.names();
  const platform = saved.info.platform;
  $(group.container).replaceChildren(...names.map((name, i) => {
    const value = list[i] || "";
    const recording = rec?.group === path && rec?.slot === i;
    let body;
    if (recording) {
      const held = MOD_ORDER.filter((m) => rec.mods.has(m));
      const hint = !rec.ready ? t("gui.hotkey.pausing") : rec.mode === "os" ? t("gui.hotkey.press_os") : t("gui.hotkey.press");
      body = held.length ? keycaps(held, platform) : el("span", { class: "hint" }, hint);
    } else if (value) {
      body = keycaps(value.split("+"), platform);
    } else {
      body = el("span", { class: "hint" }, i === 0 ? t("gui.hotkey.click_to_record") : t("gui.hotkey.unset_click"));
    }
    return el("div", { class: "slot" },
      el("span", { class: "slot-name" }, name),
      el("button", {
        type: "button",
        class: recording ? "recorder recording" : "recorder",
        "aria-label": tf("gui.hotkey.slot_aria", {
          name,
          value: value || t("gui.hotkey.unset"),
          action: recording ? t("gui.hotkey.recording") : t("gui.hotkey.click_to_record"),
        }),
        onclick: () => (recording ? endRecording() : startRecording(path, i)),
      },
        recording ? el("span", { class: "rec-dot" }) : null,
        body,
        recording ? el("span", { class: "rec-hint" }, t("gui.hotkey.esc_cancel")) : null),
      // No hotkey at all is a valid setup, so every slot can be cleared.
      value && !recording
        ? el("button", {
          type: "button",
          class: "clear",
          title: tf("gui.hotkey.clear", { name }),
          "aria-label": tf("gui.hotkey.clear", { name }),
          onclick: () => setSlot(path, i, ""),
        }, "×")
        : el("span")); // keeps the grid column
  }));
  if (!group.extra) return;
  const extra = list.slice(names.length);
  $(group.extra).hidden = extra.length === 0;
  $(group.extra).textContent = tf("gui.hotkey.extra", { combos: extra.join(t("gui.list_separator")) });
}

function setSlot(path, i, value) {
  const list = splitHotkeys(getPath(draft, path));
  // A combination means one thing, in either group.
  const taken = Object.keys(HOTKEY_GROUPS).flatMap((k) =>
    splitHotkeys(getPath(draft, k)).filter((v, j) => !(k === path && j === i)));
  if (value && taken.includes(value)) {
    showError(path, tf("gui.hotkey.taken", { combo: value }));
    return;
  }
  list[i] = value;
  setPath(draft, path, list.filter(Boolean).join(", "));
  if (path === "dictation.hotkeys") $("hotkeysManual").value = draft.dictation.hotkeys;
  showError(path, "");
  renderSlots();
  updateDirty();
}

async function startRecording(group, slot) {
  endRecording();
  showError(group, "");
  const r = { group, slot, mode: saved.shell === "app" ? "os" : "keys", mods: new Set(), ready: false, keepalive: 0 };
  rec = r;
  if (r.mode === "os") {
    r.ready = true;
    renderSlots();
    recordWithOs(r);
    return;
  }
  renderSlots();
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
      showError(r.group, err.message);
      return;
    }
    // No OS recorder here (a permission, a backend without one): record
    // the keys this page sees instead.
    r.mode = "keys";
    r.ready = false;
    renderSlots();
    await pauseHotkeys(r);
    return;
  }
  if (rec !== r) return;
  endRecording();
  if (value) acceptCombo(r.group, r.slot, value);
}

// Keeps the global hotkeys paused while this page records keys, so the old
// hotkey does not start a session.
async function pauseHotkeys(r) {
  try {
    await api("/api/hotkey/suspend", { active: true });
  } catch (err) {
    if (rec === r) endRecording();
    showError(r.group, err.message);
    return;
  }
  if (rec !== r) return;
  r.ready = true;
  r.keepalive = setInterval(() => api("/api/hotkey/suspend", { active: true }).catch(() => {}), 5000);
  renderSlots();
}

function endRecording() {
  if (!rec) return;
  const r = rec;
  rec = null;
  clearInterval(r.keepalive);
  if (r.mode === "keys") api("/api/hotkey/suspend", { active: false }).catch(() => {});
  renderSlots();
}

// The server checks the combination's form; what is taken is decided against
// the draft, in setSlot, so both groups are checked as dictation hotkeys.
async function acceptCombo(group, slot, value) {
  try {
    setSlot(group, slot, (await api("/api/hotkey/check", { value, role: "dictation" })).value);
  } catch (err) {
    showError(group, err.message);
  }
}

// A non-modifier key completes the combination at once; a modifier-only one
// completes when its first modifier is released, with every modifier held.
function finishRecording(key) {
  const { group, slot, mods } = rec;
  const value = [...MOD_ORDER.filter((m) => mods.has(m)), ...(key ? [key] : [])].join("+");
  endRecording();
  acceptCombo(group, slot, value);
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
    renderSlots();
    return;
  }
  if (ev.code === "Escape" && rec.mods.size === 0) {
    endRecording();
    return;
  }
  const key = keyName(ev.code);
  if (!key) {
    showError(rec.group, tf("gui.hotkey.unrecordable", { code: ev.code || ev.key }));
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

$("manualToggle").addEventListener("click", () => {
  const input = $("hotkeysManual");
  input.hidden = !input.hidden;
  $("manualToggle").textContent = input.hidden ? t("gui.hotkey.manual") : t("gui.hotkey.manual_hide");
  if (!input.hidden) input.focus();
});

// ---- devices ----
//
// Recording your voice and recording what the computer plays are different
// jobs on different devices; group them so the choice is obvious. A
// configured device that is not plugged in now stays selectable, so opening
// the settings never rewrites the config.

function deviceOptions(value) {
  const devices = saved.devices;
  const current = /^default$/i.test(value) ? "" : value;
  const match = current ? devices.find((d) => d.id === current || d.name === current) : null;
  const options = [el("option", { value: "" }, t("gui.device.default"))];
  const groups = [["microphone", t("gui.device.group_microphone")], ["loopback", t("gui.device.group_loopback")]];
  for (const [kind, label] of groups) {
    const items = devices.filter((d) => d.kind === kind);
    if (items.length) {
      options.push(el("optgroup", { label }, ...items.map((d) => el("option", { value: d === match ? current : d.id }, d.name))));
    }
  }
  if (current && !match) options.push(el("option", { value: current }, tf("gui.device.absent", { name: current })));
  return { options, selected: current };
}

function renderDevices() {
  for (const [id, path] of [["device", "dictation.device"], ["continuousDevice", "continuous.device"]]) {
    const { options, selected } = deviceOptions(getPath(draft, path));
    $(id).replaceChildren(...options);
    $(id).value = selected;
  }
  $("loopbackNote").hidden = !(saved.info.platform === "macos" && !saved.devices.some((d) => d.kind === "loopback"));
  for (const node of document.querySelectorAll(".devices-error")) {
    node.textContent = saved.devices_error || "";
    node.hidden = !saved.devices_error;
  }
}

function renderModes() {
  const modes = [["toggle", t("gui.mode.toggle")], ["hold", t("gui.mode.hold")]];
  $("modes").replaceChildren(...modes.map(([m, label]) => el("button", {
    type: "button",
    role: "radio",
    "aria-checked": String(draft.dictation.mode === m),
    onclick: () => { draft.dictation.mode = m; renderModes(); updateDirty(); },
  }, label)));
}

// ---- hotwords ----

function renderHotwords() {
  const input = $("hotwordInput");
  $("hotwords").replaceChildren(...draft.recognition.hotwords.map((w, i) => el("span", { class: "chip" },
    w,
    el("button", {
      type: "button",
      title: tf("gui.hotwords.remove", { word: w }),
      "aria-label": tf("gui.hotwords.remove", { word: w }),
      onclick: () => removeHotword(i),
    }, "×"))), input);
}

function addHotwords(text) {
  const words = text.split(/[,，、\n]/).map((w) => w.trim()).filter(Boolean);
  const list = draft.recognition.hotwords;
  const before = list.length;
  for (const w of words) if (!list.includes(w)) list.push(w);
  if (list.length === before) return;
  renderHotwords();
  updateDirty();
}

function removeHotword(i) {
  draft.recognition.hotwords.splice(i, 1);
  renderHotwords();
  updateDirty();
  $("hotwordInput").focus();
}

$("hotwords").addEventListener("click", (ev) => { if (ev.target === ev.currentTarget) $("hotwordInput").focus(); });
$("hotwordInput").addEventListener("keydown", (ev) => {
  const input = ev.target;
  if (ev.isComposing) return; // IME candidate selection
  if (ev.key === "Enter" || ev.key === "," || ev.key === "，" || ev.key === "、") {
    ev.preventDefault();
    addHotwords(input.value);
    input.value = "";
  } else if (ev.key === "Backspace" && input.value === "" && draft.recognition.hotwords.length) {
    removeHotword(draft.recognition.hotwords.length - 1);
  }
});
$("hotwordInput").addEventListener("paste", (ev) => {
  const text = ev.clipboardData?.getData("text") || "";
  if (!/[,，、\n]/.test(text)) return;
  ev.preventDefault();
  addHotwords(ev.target.value + text);
  ev.target.value = "";
});
$("hotwordInput").addEventListener("blur", (ev) => {
  if (!ev.target.value.trim()) return;
  addHotwords(ev.target.value);
  ev.target.value = "";
});

// The desktop window picks the hotwords file with the native dialog; a
// browser cannot hand over a picked file's path, so --web types it.
function renderHotwordsFile() {
  const app = saved.shell === "app";
  const input = $("hotwordsFile");
  const path = draft.recognition.hotwords_file;
  input.value = path;
  input.readOnly = app;
  input.title = app ? path : "";
  input.placeholder = app ? t("gui.hotwords_file.none") : t("gui.hotwords_file.example");
  $("hotwordsFilePick").hidden = !app;
  $("hotwordsFileClear").hidden = !app || !path;
}

function setHotwordsFile(path) {
  draft.recognition.hotwords_file = path;
  renderHotwordsFile();
  updateDirty();
}

async function pickHotwordsFile() {
  try {
    const { path } = await api("/api/pick-file", { path: draft.recognition.hotwords_file });
    if (path) setHotwordsFile(path);
  } catch (err) {
    setMessage(err.message, "error");
  }
}

$("hotwordsFilePick").addEventListener("click", pickHotwordsFile);
$("hotwordsFile").addEventListener("click", (ev) => { if (ev.target.readOnly) pickHotwordsFile(); });
$("hotwordsFileClear").addEventListener("click", () => setHotwordsFile(""));

// ---- engines ----

function engineSettings(id) {
  return draft.recognition.engines[id] || {};
}

function renderEngines() {
  $("engines").replaceChildren(...saved.engines.map((e) => {
    const [stateClass, stateText] = e.custom
      ? ["state off", t("gui.engine.unavailable")]
      : ready[e.id] ? ["state ok", t("gui.engine.ready")] : ["state missing", t("gui.engine.missing")];
    return el("div", {
      class: "engine",
      role: "radio",
      tabindex: "0",
      "aria-checked": String(draft.recognition.engine === e.id),
      onclick: (ev) => { if (!ev.target.closest("button")) pickEngine(e); },
      onkeydown: (ev) => {
        if (ev.target !== ev.currentTarget) return;
        if (ev.key === " " || ev.key === "Enter") { ev.preventDefault(); pickEngine(e); }
      },
    },
      el("span", { class: "radio" }),
      el("span", { class: "title" },
        el("span", { class: "name" }, e.title),
        e.custom ? null : el("span", { class: "tag" }, e.streaming ? t("gui.engine.streaming") : t("gui.engine.batch"))),
      el("span", { class: stateClass }, stateText),
      e.fields.length
        ? el("button", { type: "button", class: "ghost configure", onclick: () => openSheet(e) }, t("gui.engine.configure"))
        : el("span"),
      el("span", { class: "blurb" }, e.blurb),
    );
  }));
}

function pickEngine(engine) {
  const previous = draft.recognition.engine;
  draft.recognition.engine = engine.id;
  showError("recognition.engine", "");
  renderEngines();
  updateDirty();
  // Picking an engine that still needs settings asks for them; Cancel puts
  // the previous choice back.
  if (!ready[engine.id] && engine.fields.length) openSheet(engine, previous);
}

// Whether the draft settings of engine `id` are enough, asked of Anya only
// when they differ from the saved ones.
async function refreshReady(id) {
  const settings = engineSettings(id);
  if (canonical(settings) === canonical(saved.config.recognition.engines[id] || {})) {
    ready[id] = saved.engines.find((e) => e.id === id)?.ready ?? false;
  } else {
    try {
      ready[id] = (await api("/api/engine/ready", { id, settings })).ready;
    } catch {
      return;
    }
  }
  renderEngines();
}

// ---- engine sheet ----
//
// A sheet edits one engine's fields on a scratch copy; Done puts them in the
// draft (saved with the rest of the page), Cancel drops them.

let sheet = null; // { engine, values, revertTo }

function fieldInput(f, value, onChange) {
  if (f.kind === "choice") {
    // A configured value outside the options stays selectable.
    const values = !value || f.options.includes(value) ? f.options : [...f.options, value];
    const select = el("select", { onchange: onChange },
      el("option", { value: "" }, t("gui.sheet.default")),
      ...values.map((o) => el("option", { value: o }, o)));
    select.value = value;
    return select;
  }
  if (f.kind === "secret") {
    const input = el("input", { type: "password", value, spellcheck: "false", oninput: onChange, autocomplete: "off" });
    return el("span", { class: "secret" }, input, el("button", {
      type: "button",
      class: "ghost",
      onclick: (ev) => {
        const show = input.type === "password";
        input.type = show ? "text" : "password";
        ev.target.textContent = show ? t("gui.sheet.hide") : t("gui.sheet.show");
      },
    }, t("gui.sheet.show")));
  }
  return el("input", { type: "text", value, spellcheck: "false", oninput: onChange, placeholder: t("gui.sheet.default") });
}

function sheetRow(f) {
  return el("label", { class: "row" },
    el("span", { class: "label" },
      el("span", {}, f.label, f.required && !f.advanced ? el("span", { class: "req" }, "*") : null),
      f.help ? el("small", {}, f.help) : null),
    fieldInput(f, sheet.values[f.key], (ev) => {
      sheet.values[f.key] = ev.target.value;
      renderSheetAdvancedLabel();
    }));
}

function renderSheetAdvancedLabel() {
  const filled = sheet.engine.fields.filter((f) => f.advanced && String(sheet.values[f.key]).trim()).length;
  $("sheetAdvancedLabel").textContent = filled ? tf("gui.sheet.advanced_filled", { count: filled }) : t("gui.sheet.advanced");
}

function openSheet(engine, revertTo) {
  const settings = engineSettings(engine.id);
  sheet = { engine, revertTo, values: Object.fromEntries(engine.fields.map((f) => [f.key, settings[f.key] ?? ""])) };
  const s = sheet;
  $("sheetTitle").replaceChildren(engine.title, el("span", { class: "tag" }, engine.streaming ? t("gui.engine.streaming") : t("gui.engine.batch")));
  $("sheetBlurb").textContent = engine.blurb;
  const main = engine.fields.filter((f) => !f.advanced);
  const advanced = engine.fields.filter((f) => f.advanced);
  $("sheetFields").replaceChildren(...main.map(sheetRow));
  $("sheetFields").hidden = main.length === 0;
  $("sheetAdvancedFields").replaceChildren(...advanced.map(sheetRow));
  $("sheetAdvanced").hidden = advanced.length === 0;
  $("sheetAdvanced").open = main.length === 0;
  // Open the advanced fields when they are what makes the engine ready (an
  // older kind of credentials), so nothing that matters is folded away.
  if (main.length && advanced.length && ready[engine.id]) {
    const mainOnly = Object.fromEntries(main.map((f) => [f.key, s.values[f.key]]));
    api("/api/engine/ready", { id: engine.id, settings: mainOnly })
      .then((r) => { if (sheet === s && !r.ready) $("sheetAdvanced").open = true; })
      .catch(() => {});
  }
  renderSheetAdvancedLabel();
  $("sheetDoc").hidden = !engine.docs;
  const backdrop = $("sheet");
  backdrop.hidden = false;
  requestAnimationFrame(() => backdrop.classList.add("open"));
  (backdrop.querySelector("input, select") || $("sheetDone")).focus();
}

async function closeSheet(commit) {
  if (!sheet) return;
  const { engine, values, revertTo } = sheet;
  sheet = null;
  const backdrop = $("sheet");
  backdrop.classList.remove("open");
  setTimeout(() => { if (!sheet) backdrop.hidden = true; }, 220);
  if (commit) {
    // Keys the manifest does not list are kept as they are.
    const next = { ...engineSettings(engine.id) };
    for (const f of engine.fields) {
      const v = String(values[f.key]).trim();
      if (v) next[f.key] = v;
      else delete next[f.key];
    }
    if (Object.keys(next).length || saved.config.recognition.engines[engine.id]) draft.recognition.engines[engine.id] = next;
    else delete draft.recognition.engines[engine.id];
    renderEngines();
    updateDirty();
    await refreshReady(engine.id);
  } else if (revertTo !== undefined) {
    draft.recognition.engine = revertTo;
    renderEngines();
    updateDirty();
  }
}

function openDoc(url) {
  if (saved.shell === "app") api("/api/open", { url }).catch((err) => setMessage(err.message, "error"));
  else window.open(url, "_blank", "noopener");
}

$("sheetCancel").addEventListener("click", () => closeSheet(false));
$("sheetDone").addEventListener("click", () => closeSheet(true));
$("sheetDoc").addEventListener("click", () => { if (sheet?.engine.docs) openDoc(sheet.engine.docs); });
$("sheet").addEventListener("click", (ev) => { if (ev.target === ev.currentTarget) closeSheet(false); });
$("sheet").addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") { ev.preventDefault(); closeSheet(false); }
  if (ev.key === "Enter" && ev.target.tagName === "INPUT") { ev.preventDefault(); closeSheet(true); }
});

// ---- save ----

function isDirty() {
  return !!saved && canonical(draft) !== canonical(saved.config);
}

// After a save the outcome message stays; an edit replaces it.
function updateDirty({ message = true } = {}) {
  const dirty = isDirty();
  $("save").disabled = !dirty || saving;
  $("revert").disabled = !dirty || saving;
  if (!message) return;
  if (dirty) setMessage(t("gui.save.dirty"), "");
  else if ($("message").dataset.kind === "") setMessage("", "");
}

function setMessage(text, kind) {
  const m = $("message");
  m.textContent = text;
  m.title = text;
  m.className = kind ? `message ${kind}` : "message";
  m.dataset.kind = kind;
}

// Puts each refusal under the fields it is about and shows the first.
function showInvalid(list) {
  const byField = new Map();
  for (const { fields, message } of list) {
    for (const f of fields) byField.set(f, [...(byField.get(f) || []), message]);
  }
  let first = null;
  for (const [field, messages] of byField) {
    const nodes = errorNodes(field);
    if (!nodes.length) continue;
    showError(field, messages.join(t("gui.list_separator")));
    for (const node of form.querySelectorAll(`[data-path="${field}"]`)) node.classList.add("invalid");
    first ??= nodes[0];
  }
  if (first) {
    showPane(first.closest(".pane").id.replace("pane-", ""));
    first.scrollIntoView({ block: "center", behavior: "smooth" });
  }
}

const FLASH = "anya.flash";

async function save() {
  if (!isDirty() || saving) return;
  saving = true;
  updateDirty({ message: false });
  clearErrors();
  try {
    const res = await api("/api/settings", draft);
    // A new interface language: load the page again in it.
    if (res.state.locale !== LOCALE) {
      try { sessionStorage.setItem(FLASH, JSON.stringify(res.warning ? ["warn", res.warning] : ["ok", ""])); } catch {}
      location.reload();
      return;
    }
    applyState(res.state);
    if (res.warning) setMessage(res.warning, "warn");
    else setMessage(t("gui.save.saved"), "ok");
  } catch (err) {
    setMessage(err.message, "error");
    if (err.data?.invalid) showInvalid(err.data.invalid);
  } finally {
    saving = false;
    updateDirty({ message: false });
  }
}

function showFlash() {
  let flash = null;
  try {
    flash = JSON.parse(sessionStorage.getItem(FLASH) || "null");
    sessionStorage.removeItem(FLASH);
  } catch {}
  if (!flash) return;
  const [kind, text] = flash;
  setMessage(kind === "ok" ? t("gui.save.saved") : text, kind);
}

function onEdit(ev) {
  const node = ev.target;
  if (!saved || !node.dataset.path) return;
  setPath(draft, node.dataset.path, readControl(node));
  node.classList.remove("invalid");
  if (node.dataset.path === "dictation.hotkeys") {
    showError("dictation.hotkeys", "");
    renderSlots();
  }
  updateDirty();
}

form.addEventListener("input", onEdit);
form.addEventListener("change", onEdit);
form.addEventListener("submit", (ev) => { ev.preventDefault(); save(); });
$("save").addEventListener("click", save);
$("revert").addEventListener("click", () => { applyState(saved); setMessage("", ""); });
document.addEventListener("keydown", (ev) => {
  if ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === "s") {
    ev.preventDefault();
    save();
  }
});

// ---- live status and activity ----

function timeOf(ms) {
  return new Date(ms).toLocaleTimeString(LOCALE, { hour12: false });
}

function logItem(e) {
  const time = el("time", { datetime: new Date(e.at).toISOString() }, timeOf(e.at));
  const note = e.note ? el("span", { class: "note" }, e.note) : null;
  if (e.kind === "final") {
    return el("li", { class: "ok" }, time, el("span", { class: "lv" }),
      el("div", {}, el("div", { class: "transcript" }, el("span", {}, e.text), copyButton(e.text)), note));
  }
  return el("li", { class: e.level }, time, el("span", { class: "lv" }), el("span", { class: "msg" }, e.text, note));
}

let lastLogKey = null;

function renderLogs(data) {
  const s = data.status;
  const live = (s.dictation.state !== "idle" || s.finishing > 0) && s.live_text ? s.live_text : null;
  const tail = data.activity[data.activity.length - 1];
  const key = JSON.stringify([data.activity.length, tail?.at, tail?.text, live, LOCALE]);
  if (key === lastLogKey) return;
  lastLogKey = key;
  const list = $("logs");
  const atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 8;
  const items = data.activity.map(logItem);
  if (live !== null) {
    items.push(el("li", { class: "live" }, el("time", {}, timeOf(Date.now())), el("span", { class: "lv" }),
      el("span", { class: "msg" }, tf("gui.live", { text: live }))));
  }
  if (!items.length) items.push(el("li", { class: "empty" }, t("gui.activity.empty")));
  list.replaceChildren(...items);
  if (atBottom) list.scrollTop = list.scrollHeight;
}

function renderStatus(data) {
  renderPill(data);
  renderStats(data.stats);
  $("streamState").className = `field-note state ${data.subscription.state}`;
  $("streamState").textContent = data.subscription.line;
  const on = data.status.continuous_on;
  $("continuousRunning").className = on ? "state ok" : "state off";
  $("continuousRunning").textContent = on ? t("gui.continuous.running") : t("gui.continuous.stopped");
  renderLogs(data);
}

async function poll() {
  if (!document.hidden) {
    try {
      renderStatus(await api("/api/status"));
    } catch {
      renderOffline();
    }
  }
  setTimeout(poll, 250);
}

// ---- the macOS window ----
//
// The desktop window's title bar is hidden behind the header; dragging the
// header moves the window and a double click zooms it, as a title bar does.

const header = $("header");
const INTERACTIVE = "button, input, select, textarea, a, label, [role=tab]";
header.addEventListener("mousedown", (ev) => {
  if (ev.button !== 0 || !document.body.classList.contains("mac-app") || !window.ipc) return;
  if (ev.target.closest(INTERACTIVE)) return;
  window.ipc.postMessage("drag");
});
header.addEventListener("dblclick", (ev) => {
  if (!document.body.classList.contains("mac-app") || !window.ipc) return;
  if (ev.target.closest(INTERACTIVE)) return;
  window.ipc.postMessage("zoom");
});

// ---- start ----

async function boot() {
  let pane = PANES[0];
  try { pane = localStorage.getItem("anya.pane") || pane; } catch {}
  showPane(pane);
  try {
    await loadText();
  } catch (err) {
    // Without the catalog there is only the server's own (translated) word.
    $("loadError").textContent = err.message;
    $("loadError").hidden = false;
    return;
  }
  try {
    applyState(await api("/api/state"));
  } catch (err) {
    $("loadError").textContent = tf("gui.load_failed", { error: err.message });
    $("loadError").hidden = false;
  }
  showFlash();
  poll();
}

// The tray panel, the TUI or a hand edit can change the config while this
// page is open. Take the saved settings again on focus; with unsaved edits
// only the two switches follow, and only if they were not edited here.
window.addEventListener("focus", async () => {
  if (!saved || saving || sheet || rec) return;
  let fresh;
  try { fresh = await api("/api/state"); } catch { return; }
  if (saving || sheet || rec) return;
  if (!isDirty()) {
    if (!reloadIfLocaleChanged(fresh)) applyState(fresh);
    return;
  }
  for (const path of ["dictation.enabled", "continuous.enabled"]) {
    if (getPath(draft, path) === getPath(saved.config, path)) {
      setPath(draft, path, getPath(fresh.config, path));
      writeControl(form.querySelector(`[data-path="${path}"]`), getPath(draft, path));
    }
    setPath(saved.config, path, getPath(fresh.config, path));
  }
  updateDirty();
});

boot();
