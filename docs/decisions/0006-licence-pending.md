# 0006: The licence is not chosen yet

Accepted, 2026-10-30.

## Context

Anya may want a permissive licence so others can embed the core, or a copyleft one. Whatever enters the repository now must not decide that by accident.

## Decision

Until a licence is chosen:

- Every crate is `publish = false` and carries no licence field.
- Code is written for Anya. Code from another project comes in only when its authors agreed or its licence allows every choice still open.
- Dependencies are permissive only (deny.toml).

## Consequences

The licence stays an open choice; choosing it supersedes this record.
