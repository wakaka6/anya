# 0017: The project's own plugins are built into Anya

Accepted, 2026-11-03.

## Context

0010 and 0013 distribute every plugin through the registry: the market fetches the index, an install downloads a tarball. The plugins the Anya project writes (`spoken-punctuation`, `app-hotwords`, `daily-notes`) live in this repository and are reviewed with Anya itself. Fetching them from the network adds nothing a user can check, and leaves them out of the market whenever the registry is unreachable. Marking them "official" in the index would need a list the registry's CI trusts, kept apart from the packages so a community plugin cannot claim it.

## Decision

- An official plugin is compiled into the `anya` binary from `plugins/<id>/`: its `package.json` and the files that file's `files` lists (`crates/anya/src/official.rs`). Being built in is what "official" means; the index has no such field.
- The market lists the official plugins first, tagged "Official", with or without the registry. A registry entry with an official plugin's id is not listed.
- Installing an official plugin unpacks it from the binary through the same checks and unpacking as a download (`Store::install_bundled`), recorded disabled. Install and enable stay separate user actions; nothing installs by itself.
- An installed official plugin is the one this Anya ships. When Anya starts, each installed official plugin whose files differ from the binary's is replaced by them (`Store::update_bundled`), on or off as the user left it, and the activity list says which changed. magpie keeps its own plugins (`@magpie-community/*`) up to date by itself the same way while other plugins' updates wait for the user; Anya's need no network because the new version arrives with Anya.
- An official package is marked `"private": true`, and the plugin-release workflow refuses to release a private package. A test keeps the two lists equal.

## Consequences

- The official plugins install offline and change only with an Anya release; a fix to one waits for the next Anya version. Going back to an older Anya brings its older official plugins back too.
- A plugin never installed stays uninstalled: an upgrade updates, it does not add.
- Community plugins are unchanged: reviewed by pull request, released by tag through the registry (0010, 0013).
