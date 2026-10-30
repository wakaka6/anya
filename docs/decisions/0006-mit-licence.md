# 0006: Anya is MIT licensed

Accepted, 2026-10-30.

## Context

Anya is meant to be embedded, extended and built on by others: plugins, downstream apps, and programs that read its frames. A permissive licence keeps that free of conditions.

## Decision

- The repository is under the MIT licence (`LICENSE`); every crate says `license = "MIT"` through the workspace.
- Code from another project comes in only when its licence is compatible with MIT or its authors agreed.
- Dependencies are permissive only (`deny.toml`). A copyleft dependency needs the owner's say, as `option-ext` (MPL-2.0, unmodified) has.
- Crates stay `publish = false` until the first release.

## Consequences

- Contributions are accepted under the same licence.
- Changing the licence later needs every contributor's agreement, so it is decided once, here.
