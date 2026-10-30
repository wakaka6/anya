# Anya domain vocabulary

## ASR Gateway

The Anya process that accepts audio from local or authorized remote clients, sends it to the selected recognition engine, and returns recognition events. The gateway owns engine credentials and provider selection.

## Recognition session

One bounded attempt to turn one audio input into text. A session receives audio, may publish partial results, ends with a final result or an explicit error, cancellation, or closure.

## Remote client

A paired program or device that connects to Anya's gateway and can use the scopes assigned to its token. An iPhone, browser client, and desktop application are all remote clients when they use the gateway connection rather than Anya's local process interfaces.

## Streaming output

Recognition results arrive while audio is still being sent. Partial results have `final=false`; the last result has `final=true`.

## Batch upstream

A recognition engine that accepts audio but produces no result until the session is finished. The gateway continues accepting audio while the client records, then returns the final result after stop.

## LAN sharing

The explicit setting that enables Anya's authenticated TLS listener beyond loopback. It is off by default and can be reached across networks through a user-managed VPN.

## Plugin vocabulary

## Provider plugin

An Engine plugin that connects Anya to an ASR provider. It exposes a Manifest, receives recognition-session audio, and returns streaming or final results.

## Plugin registry

The reviewed catalog from which users discover and install plugins. `anyahq-plugins` publishes indexed releases; a registry update does not install or enable a plugin by itself.
