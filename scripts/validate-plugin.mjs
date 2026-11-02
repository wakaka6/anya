// Registry CI: validates a plugin release and merges its entry into index.json.
//   bun scripts/validate-plugin.mjs <package-dir> <tarball.tgz> <artifact-url> <index.json> <sdk/index.js> <icons-dir> <icons-url>
// <package-dir> is the unpacked tarball (its package.json at the root);
// <sdk/index.js> is the @wakaka6/anya-plugin SDK of the Anya release the plugin targets.
// The icon "anya.icon" names is checked as Anya checks it and written to
// <icons-dir> under a name made from its content; the index entry points at
// it under <icons-url>, so the market shows it before install.
//
// A package has "anya.host" (runs in Bun), "anya.inline" (runs in QuickJS
// inside Anya), or both. The script loads each entry and writes what they
// export into the index entry as `provides`, so the two cannot disagree
// (docs/decisions/0015-plugin-runtimes.md).
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, existsSync, mkdirSync } from "node:fs";
import { pathToFileURL } from "node:url";
import path from "node:path";

const [dir, tarball, artifact, indexFile, sdkFile, iconsDir, iconsUrl] = process.argv.slice(2);
const PROTOCOL = 1;
const KINDS = ["engine", "context", "transform", "action"];
const LIMIT_KEYS = ["inline_ms", "inline_memory_mib", "context_ms", "transform_ms", "action_ms"];
const FIELD_KINDS = ["text", "secret", "choice"];
const problems = [];
const need = (ok, message) => ok || problems.push(message);

const pkg = JSON.parse(readFileSync(path.join(dir, "package.json"), "utf8"));
const anya = pkg.anya ?? {};
const semver = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/;
const id = anya.id ?? pkg.name;
need(/^[a-z0-9-]+$/.test(id ?? ""), `plugin id ${JSON.stringify(id)} must be lowercase letters, digits and '-'`);
need(semver.test(pkg.version ?? ""), `version ${JSON.stringify(pkg.version)} is not semver`);
need(
  typeof anya.host === "string" || typeof anya.inline === "string",
  'package.json needs "anya": { "host": "./hooks.js" } and/or { "inline": "./inline.js" }',
);
need(anya.host === undefined || typeof anya.host === "string", '"anya.host" must be a file name');
need(anya.inline === undefined || typeof anya.inline === "string", '"anya.inline" must be a file name');
need(typeof anya.compat === "string", 'package.json needs "anya": { "compat": ">=0.1.0" }, the Anya versions it runs on');
need(anya.protocol === PROTOCOL, `"anya.protocol" must be ${PROTOCOL}`);
const permissions = anya.permissions ?? [];
need(
  Array.isArray(permissions) && permissions.every((p) => /^(network|env|fs):[^\s]+$/.test(p)),
  'permissions are strings like "network:api.example.com", "env:API_KEY", "fs:read:/path"',
);
need(!/^v?\d+$/.test(anya.compat ?? "") && !/\*\s*$/.test(anya.compat ?? "") && anya.compat !== "*", "compat must not accept every Anya");

const icon = anya.icon === undefined ? undefined : readIcon(anya.icon);

const fields = anya.fields ?? [];
need(Array.isArray(fields), '"anya.fields" must be a list');
if (Array.isArray(fields)) validateFields(fields, '"anya.fields"');
const limits = anya.limits ?? {};
need(limits !== null && typeof limits === "object" && !Array.isArray(limits), '"anya.limits" must be an object');
if (limits !== null && typeof limits === "object") {
  for (const [key, value] of Object.entries(limits)) {
    need(LIMIT_KEYS.includes(key), `"anya.limits.${key}" is not a limit; use ${LIMIT_KEYS.join(", ")}`);
    need(Number.isInteger(value) && value > 0, `"anya.limits.${key}" must be a positive whole number`);
  }
}

function validateFields(list, where) {
  const keys = new Set();
  for (const f of list) {
    need(typeof f?.key === "string" && f.key !== "", `${where}: every field needs a key`);
    need(!keys.has(f?.key), `${where}: field ${f?.key} appears twice`);
    keys.add(f?.key);
    need(f?.label !== undefined && f.label !== "", `${where}: field ${f?.key} needs a label`);
    need(FIELD_KINDS.includes(f?.kind), `${where}: field ${f?.key} has kind ${JSON.stringify(f?.kind)}; use ${FIELD_KINDS.join(", ")}`);
    need(f?.kind !== "choice" || Array.isArray(f?.options), `${where}: field ${f?.key}: kind "choice" needs options`);
  }
}

// The icon's bytes and type, checked as Anya checks them (crates/plugin-host/src/icon.rs).
function readIcon(name) {
  if (typeof name !== "string" || name.trim() === "") {
    problems.push('"anya.icon" must be a file in the package or a data:image URI');
    return undefined;
  }
  let bytes;
  const trimmed = name.trim();
  if (/^data:image\//i.test(trimmed)) {
    const comma = trimmed.indexOf(",");
    const body = comma < 0 ? "" : trimmed.slice(comma + 1);
    bytes = /;base64$/i.test(trimmed.slice(0, Math.max(comma, 0)))
      ? Buffer.from(body.replace(/\s/g, ""), "base64")
      : Buffer.concat(body.split(/(%[0-9a-fA-F]{2})/).map((part) =>
          /^%[0-9a-fA-F]{2}$/.test(part) ? Buffer.from([parseInt(part.slice(1), 16)]) : Buffer.from(part)));
  } else if (trimmed.includes("://")) {
    problems.push('"anya.icon" must be a file in the package or a data:image URI, not a URL');
    return undefined;
  } else {
    const file = path.resolve(dir, trimmed);
    if (path.isAbsolute(trimmed) || path.relative(path.resolve(dir), file).startsWith("..")) {
      problems.push('"anya.icon" lies outside the package');
      return undefined;
    }
    if (!existsSync(file)) {
      problems.push(`"anya.icon" ${trimmed} is not a file in the package`);
      return undefined;
    }
    bytes = readFileSync(file);
  }
  const type = sniffIcon(bytes);
  need(bytes.length > 0, '"anya.icon" is empty');
  need(bytes.length <= 1 << 20, '"anya.icon" is larger than 1 MiB');
  need(type !== undefined, '"anya.icon" is not PNG, JPEG, GIF, WebP, ICO or SVG');
  if (type === "svg") {
    const text = bytes.toString("utf8");
    need(!/<script[\s>]/i.test(text) && !/\son\w+\s*=/i.test(text), '"anya.icon" is an SVG with scripts or event handlers');
    need(!/(href|src)\s*=\s*["']\s*(https?:)?\/\//i.test(text), '"anya.icon" is an SVG that loads something from outside it');
  }
  return { bytes, type };
}

function sniffIcon(b) {
  const starts = (...sig) => sig.every((x, i) => b[i] === x);
  if (starts(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)) return "png";
  if (starts(0xff, 0xd8, 0xff)) return "jpg";
  if (b.subarray(0, 6).toString("latin1") === "GIF87a" || b.subarray(0, 6).toString("latin1") === "GIF89a") return "gif";
  if (b.length >= 12 && b.subarray(0, 4).toString("latin1") === "RIFF" && b.subarray(8, 12).toString("latin1") === "WEBP") return "webp";
  if (starts(0, 0, 1, 0)) return "ico";
  if (b.subarray(0, 1024).toString("latin1").toLowerCase().includes("<svg")) return "svg";
  return undefined;
}

// An entry's file, which must lie inside the package.
function entryFile(entry, what) {
  const file = path.resolve(dir, entry);
  if (path.relative(path.resolve(dir), file).startsWith("..")) {
    problems.push(`the ${what} entry lies outside the package`);
    return undefined;
  }
  if (!existsSync(file)) {
    problems.push(`the ${what} entry ${entry} does not exist`);
    return undefined;
  }
  return file;
}

// What QuickJS cannot do: the inline entry is one file with nothing to load.
const NOT_INLINE = [
  [/^\s*import\s*[\w{*"'$]/m, "an `import` statement"],
  [/\bimport\s*\(/, "a dynamic `import(`"],
  [/\brequire\s*\(/, "a `require(`"],
  [/^\s*export\s[^;\n]*\sfrom\s*["']/m, "an `export … from`"],
];

const provides = new Set();
let engine;

if (problems.length === 0) {
  const sdkUrl = pathToFileURL(path.resolve(sdkFile)).href;
  const sdk = await import(sdkUrl);
  Bun.plugin({
    name: "anya-sdk",
    setup: (b) => b.module("@wakaka6/anya-plugin", () => ({ exports: { ...sdk }, loader: "object" })),
  });

  if (anya.host !== undefined) {
    const file = entryFile(anya.host, "host");
    if (file) {
      try {
        const mod = await import(pathToFileURL(file).href);
        const found = {};
        const seen = new Set();
        for (const value of Object.values(mod)) {
          if (seen.has(value)) continue;
          seen.add(value);
          const kind = sdk.kindOf(value);
          if (!kind) continue;
          need(!found[kind], `the host entry exports two ${kind} hooks; a package has at most one of each kind`);
          found[kind] ??= value;
        }
        need(
          KINDS.some((k) => found[k]),
          "the host entry exports nothing made with defineEngine, defineContext, defineTransform or defineAction",
        );
        for (const kind of KINDS) if (found[kind]) provides.add(kind);
        engine = found.engine?.manifest;
        if (engine) {
          need(engine.id === id, `manifest.id ${JSON.stringify(engine.id)} must equal the plugin id ${JSON.stringify(id)}`);
          need(typeof engine.streaming === "boolean", "manifest.streaming must be true or false");
          validateFields(engine.fields ?? [], "manifest.fields");
        }
      } catch (e) {
        problems.push(`the host entry does not load: ${e.message}`);
      }
    }
  }

  if (anya.inline !== undefined) {
    const file = entryFile(anya.inline, "inline");
    if (file) {
      const source = readFileSync(file, "utf8");
      for (const [pattern, what] of NOT_INLINE) {
        need(!pattern.test(source), `the inline entry must be one file with nothing to load, but it contains ${what}`);
      }
      if (problems.length === 0) {
        try {
          const mod = await import(pathToFileURL(file).href);
          for (const kind of ["context", "transform"]) {
            if (mod[kind] === undefined) continue;
            need(typeof mod[kind] === "function", `the inline entry's ${kind} must be a function`);
            provides.add(kind);
          }
          need(
            mod.context !== undefined || mod.transform !== undefined,
            "the inline entry exports neither a context nor a transform function",
          );
        } catch (e) {
          problems.push(`the inline entry does not load: ${e.message}`);
        }
      }
    }
  }
}

if (problems.length) {
  for (const p of problems) console.error(`error: ${p}`);
  process.exit(1);
}

const sorted = KINDS.filter((k) => provides.has(k));
let iconEntry = {};
if (icon) {
  const sum = createHash("sha512").update(icon.bytes).digest();
  const file = `${sum.subarray(0, 16).toString("hex")}.${icon.type}`;
  mkdirSync(iconsDir, { recursive: true });
  writeFileSync(path.join(iconsDir, file), icon.bytes);
  iconEntry = { icon: `${iconsUrl.replace(/\/$/, "")}/${file}`, icon_integrity: "sha512-" + sum.toString("base64") };
}
const integrity = "sha512-" + createHash("sha512").update(readFileSync(tarball)).digest("base64");
const index = existsSync(indexFile) ? JSON.parse(readFileSync(indexFile, "utf8")) : { version: 1, plugins: [] };
let plugin = index.plugins.find((p) => p.id === id);
if (!plugin) {
  plugin = { id, name: anya.name ?? engine?.title ?? pkg.name ?? id, description: anya.description ?? pkg.description ?? engine?.blurb ?? "", versions: [] };
  index.plugins.push(plugin);
}
if (plugin.versions.some((v) => v.version === pkg.version)) {
  console.error(`error: ${id} ${pkg.version} is already in the index; versions are immutable`);
  process.exit(1);
}
plugin.versions.push({
  version: pkg.version,
  artifact,
  integrity,
  anya: anya.compat,
  protocol: PROTOCOL,
  provides: sorted,
  host: anya.host !== undefined,
  streaming: engine?.streaming ?? false,
  permissions,
  ...iconEntry,
});
writeFileSync(indexFile, JSON.stringify(index, null, 2) + "\n");
console.log(`ok: ${id} ${pkg.version} ${sorted.join(",")} ${integrity}`);
