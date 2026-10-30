# 0013: The registry publishes to a branch of its own repository

Accepted, 2026-10-30.

## Context

0010 had the registry's CI attach each tarball to a GitHub release and publish `index.json` to GitHub Pages. That is two more places to set up and keep working for what is a static file and a few archives. magpie's community registry keeps its index and package tarballs on a branch of the repository itself and reads them from raw.githubusercontent.com.

## Decision

- The registry CI commits each release's tarball to `tgz/` and the updated `index.json` to the `dist` branch of `anya-plugins`.
- Anya's default index is `https://raw.githubusercontent.com/wakaka6/anya-plugins/dist/index.json`; an index entry's `artifact` is a raw URL on the same branch.
- Nothing else in 0010 changes: review by pull request, the tag `<id>-v<version>`, validation, the sha512 `integrity` Anya checks before unpacking, immutable versions, and install, enable and update as separate user actions.

## Consequences

- No GitHub release and no GitHub Pages site to maintain.
- Raw URLs of a branch can be served stale by CDNs for a few minutes; the integrity hash in the index still guards every download, and a stale index only delays a new version.
- `[plugins] index` in the config still points Anya at a mirror or a `file://` index.
