// Registry CI: validates a plugin release and merges its entry into index.json.
//   bun scripts/validate-plugin.mjs <package-dir> <tarball.tgz> <artifact-url> <index.json> <sdk/index.js>
// <package-dir> is the unpacked tarball (its package.json at the root);
// <sdk/index.js> is the @wakaka6/anya-plugin SDK of the Anya release the plugin targets.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { pathToFileURL } from "node:url";
import path from "node:path";

const [dir, tarball, artifact, indexFile, sdkFile] = process.argv.slice(2);
const PROTOCOL = 1;
const problems = [];
const need = (ok, message) => ok || problems.push(message);

const pkg = JSON.parse(readFileSync(path.join(dir, "package.json"), "utf8"));
const semver = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/;
const id = pkg.anya?.id ?? pkg.name;
need(/^[a-z0-9-]+$/.test(id ?? ""), `plugin id ${JSON.stringify(id)} must be lowercase letters, digits and '-'`);
need(semver.test(pkg.version ?? ""), `version ${JSON.stringify(pkg.version)} is not semver`);
need(typeof pkg.anya?.engine === "string", 'package.json needs "anya": { "engine": "./engine.js" }');
need(typeof pkg.anya?.compat === "string", 'package.json needs "anya": { "compat": ">=0.1.0" }, the Anya versions it runs on');
need(pkg.anya?.protocol === PROTOCOL, `"anya.protocol" must be ${PROTOCOL}`);
const permissions = pkg.anya?.permissions ?? [];
need(
  Array.isArray(permissions) && permissions.every((p) => /^(network|env|fs):[^\s]+$/.test(p)),
  'permissions are strings like "network:api.example.com", "env:API_KEY", "fs:read:/path"',
);
need(!/^v?\d+$/.test(pkg.anya?.compat ?? "") && !/\*\s*$/.test(pkg.anya?.compat ?? "") && pkg.anya?.compat !== "*", "compat must not accept every Anya");

let manifest;
if (problems.length === 0) {
  Bun.plugin({
    name: "anya-sdk",
    setup: (b) => b.module("@wakaka6/anya-plugin", async () => ({ exports: { ...(await import(pathToFileURL(path.resolve(sdkFile)).href)) }, loader: "object" })),
  });
  try {
    const mod = await import(pathToFileURL(path.resolve(dir, pkg.anya.engine)).href);
    manifest = mod.default?.manifest;
    need(typeof mod.default?.open === "function", "the entry must export defineEngine(...) as default");
    need(manifest?.id === id, `manifest.id ${JSON.stringify(manifest?.id)} must equal the plugin id ${JSON.stringify(id)}`);
    need(typeof manifest?.streaming === "boolean", "manifest.streaming must be true or false");
    for (const f of manifest?.fields ?? []) need(f.kind !== "choice" || Array.isArray(f.options), `field ${f.key}: kind "choice" needs options`);
  } catch (e) {
    problems.push(`the engine does not load: ${e.message}`);
  }
}

if (problems.length) {
  for (const p of problems) console.error(`error: ${p}`);
  process.exit(1);
}

const integrity = "sha512-" + createHash("sha512").update(readFileSync(tarball)).digest("base64");
const index = existsSync(indexFile) ? JSON.parse(readFileSync(indexFile, "utf8")) : { version: 1, plugins: [] };
let plugin = index.plugins.find((p) => p.id === id);
if (!plugin) {
  plugin = { id, name: pkg.anya.name ?? manifest.title, description: pkg.description ?? manifest.blurb ?? "", versions: [] };
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
  anya: pkg.anya.compat,
  protocol: PROTOCOL,
  kind: "engine",
  streaming: manifest.streaming,
  permissions,
});
writeFileSync(indexFile, JSON.stringify(index, null, 2) + "\n");
console.log(`ok: ${id} ${pkg.version} ${integrity}`);
