# Anya is an opt-in ASR gateway for remote clients

Accepted, 2026-10-30.

Anya remains a local service by default: it listens on loopback or a Unix socket, and local clients may use the granted subscriber and dictation capabilities. When the user enables LAN sharing, Anya also accepts authenticated TLS WebSocket connections from clients on the local network or the user's own VPN. A paired client may upload microphone audio and receive recognition results from Anya's configured engine, so clients do not integrate individual ASR vendors or hold their credentials.

Remote dictation and local dictation use the same recognition-session path. The client sends control messages (`dictation_start`, `dictation_stop`, `dictation_cancel`) and audio chunks; Anya returns the existing session, result, level and end frames. A streaming engine returns partial results while audio is arriving. A non-streaming engine accepts audio while it arrives but returns only after `dictation_stop`; Anya never fabricates partial results for it. Remote dictation returns to its requesting client and does not paste into the gateway host by default.

LAN sharing and its permissions are explicit. A client pairs with a one-time code shown in the settings and receives a token; Anya stores only its hash. The token carries scopes (`dictate`, `subscribe_continuous`, `subscribe_dictation`; a new client holds `dictate` only); removing a client ends its active connections. The remote listener is disabled unless LAN sharing is enabled, and non-loopback connections require TLS and a token. Anya does not provide a transcript relay service; a user-managed VPN can provide access across networks.

## Consequences

- Supersedes the loopback-only rule of 0005 for the opt-in LAN listener; the loopback listener keeps it. Answers 0007's open question: an app identifies itself by its paired token, and the user grants scopes in the settings.
- `anya-server` becomes bidirectional for authenticated client sessions while retaining the existing subscriber stream.
- The protocol documents client commands and binary audio input in addition to server frames; `session_start.streaming` reports the selected engine's actual output capability.
- The engine seam remains responsible for buffering batch input; the gateway does not keep an unbounded copy of every remote recording.
- Client UIs must show “recording”, “recognizing after stop” and “live recognition” as distinct states.
