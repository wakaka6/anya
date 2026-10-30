# 0014: Updates come through Homebrew; Anya tells the user and can run the upgrade

Accepted, 2026-10-30.

## Context

magpie updates itself: it downloads, checks a signature, replaces its own binary, rolls back and restarts (about 6500 lines, half of it for Windows). Anya targets macOS and Linux, and Homebrew exists on both. A binary replaced under Homebrew's Cellar makes `brew` believe the old version is installed, so magpie itself leaves Homebrew installs to `brew upgrade`.

## Decision

- Anya ships through a Homebrew tap and never replaces its own binary. Download, integrity (the formula's sha256) and replacement are Homebrew's.
- Anya checks whether a newer version exists by reading the tap's `Formula/anya.rb` and its `version`, the version `brew upgrade` would install. It never announces a version Homebrew cannot yet give. `[updates] formula` points the check at another formula URL, `https://` or `file://`; empty is the default tap.
- The check runs at start and then once a day. It sends a GET with a `User-Agent` of `anya/<version>` and nothing else: no identifier, no config, no transcript. `[updates] check = false` turns it off, and then Anya makes no such request. It is on by default, and the settings page says what it sends (0005).
- A newer version is shown in the tray panel, the settings page and the terminal UI. For an install under a Homebrew Cellar, the notice has an Upgrade action that runs `brew upgrade anya` and reports its result. For any other install (a copied binary, `cargo install`) the notice says a new version is out and where to get it; Anya does not run anything.
- After a successful upgrade Anya says it must be restarted to run the new version. It does not restart itself.
- A failed check is shown only where the user looks for updates; it never interrupts dictation.

## Consequences

- No updater code to keep safe: no signature check, rollback or elevation.
- The upgrade runs the user's `brew` with the user's rights. Anya finds `brew` on `PATH` and in Homebrew's default prefixes; when it is missing the action reports that.
- The first request to the tap's host happens without the user choosing it; that is why it is on the record and can be turned off.
- The version comes from the tap, so it lags a release by however long the tap's CI takes, never the other way round.

## Release

- A tag `vX.Y.Z` runs `.github/workflows/release.yml`: it checks the tag equals the `anya` crate's version (Anya compares its own version with the formula's), builds macOS and Linux for arm64 and amd64, publishes `anya_<os>_<arch>.tar.gz` and `SHA256SUMS` as a release, then writes `Formula/anya.rb` (from `scripts/render-formula`) to the tap `wakaka6/homebrew-tap`. The tap is written last, so a formula never names an archive that is not published.
- The workflow needs the repository secret `TAP_TOKEN`, a token with write access to the tap.
