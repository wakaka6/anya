# ASR Providers are distributed through the anyahq-plugins registry

Accepted, 2026-10-30.

ASR providers are Engine plugins. A provider plugin implements the same `Engine` interface as a built-in provider, sends its `Manifest`, accepts recognition sessions, and returns streaming or final `Recognition`s according to `Manifest.streaming`. The ASR gateway selects a provider by its manifest id; clients do not integrate provider SDKs or store provider credentials.

`anyahq-plugins` is the public plugin registry and review source. A plugin release or tag runs registry CI, which validates the plugin manifest, compatibility range, declared permissions and release artifact, then publishes an index entry containing the plugin id, version, artifact URL, integrity hash, supported protocol/runtime versions, capabilities and permissions. Anya refreshes this index when the user opens the plugin marketplace or checks for updates; it does not run a live Git tag listener and never installs a tag automatically.

Installing, updating and enabling a plugin are separate user actions. The UI shows that an Engine plugin receives audio, its provider name, streaming capability, required credentials and requested permissions. The installed version and integrity hash are pinned locally. A plugin failure ends its session with an error; Anya does not silently switch to another provider.

## Consequences

- The plugin host remains one out-of-process Bun host, as decided in 0004; the marketplace only discovers and distributes packages.
- The gateway can expose the same provider list to desktop, web and mobile clients without exposing vendor-specific APIs.
- `Manifest.streaming` is the provider capability used to choose live partial output versus final-only output.
- Registry refresh, package download, signature/hash verification and plugin execution are separate trust steps.
