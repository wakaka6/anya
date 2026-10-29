# 0003: One frame format, in process and on the wire

Accepted, 2026-10-30.

## Context

A subscription that no built-in feature uses is a side path: nothing tells its authors when it falls short, and without a version nothing tells its readers when it changed.

## Decision

`anya-protocol` defines every format that crosses a process boundary: subscriber frames, the `Recognition` an engine reports and the `Manifest` it describes itself with. Built-in sinks read the same `Frame`s in process that subscribers read as JSON. A `result` frame carries a `Recognition` inline, so an engine plugin and a subscriber learn one shape.

The first frame on a connection is `hello` with `PROTOCOL_VERSION`. While the version stays, formats only grow; consumers ignore what they do not know. The version is 0 until the first release.

## Consequences

- Each frame's JSON is pinned by a test; changing a pin is a protocol change.
- A frame type that only one built-in needs still goes through this crate.
