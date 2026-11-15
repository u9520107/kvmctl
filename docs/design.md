# kvmctl design

Status: draft design; implementation has not started. See [execution plan](execution-plan.md) for sequencing, verification gates, and acceptance evidence. See [planning index](plan.md) for current status and questions needing user input.

## Summary

`kvmctl` is a local-first, low-latency KVM control system for discovering and switching between computers through hardware endpoints. It should provide one native Rust core for discovery, authorization, transport, input routing, and device management, with separate endpoint adapters for an ESP32-S3 implementation first and JetKVM later.

The name follows the existing `monitorctl` naming style. Planned components are:

- `kvmctl`: CLI and reusable client functionality.
- `kvmctld`: optional background service for persistent sessions and hotkeys.
- `kvmctl-agent`: ESP32-S3 firmware endpoint.
- KVM Control: optional Windows tray UI on top of the Rust core.

## Goals

- Discover compatible endpoints on the local network without static IP configuration.
- Discover devices on the LAN and authorize multiple clients independently.
- Forward keyboard and mouse input with low latency.
- Keep endpoint USB HID attached while switching targets with a hotkey; network sessions exist only for the selected target.
- Build and validate an ESP32-S3 HID-only endpoint first.
- Integrate JetKVM later through a device-specific adapter.
- Use WebRTC DataChannels for authenticated, encrypted control traffic.
- Support optional video when an endpoint can provide it; input-only operation remains valid.
- Work locally without a cloud account or mandatory public infrastructure.
- Reconnect cleanly after network changes, endpoint restarts, sleep, or resume.

Non-goals for the first milestone:

- General-purpose remote desktop software.
- Cloud relay, account system, or public device directory.
- Always-on video streaming.
- Automatic remote power control before its security and hardware behavior are understood.

## Local-first security

Local operation is the default trust boundary. Discovery may be visible on the LAN, but discovery alone must never authorize control.

### Hostile-LAN bootstrap

Assume a hotel or other open Wi-Fi can observe, spoof, modify, or block local traffic. Security must not depend on the Wi-Fi being trusted.

- Generate a persistent device identity keypair on the ESP32-S3. Use its public-key fingerprint as the endpoint identity; do not use an IP address, hostname, or MAC address as identity.
- Generate a separate keypair on each control device. Store only each client's public key on the ESP32-S3; keep private keys on their originating control devices.
- Enroll control devices over direct USB. LAN-only first-use authorization is not allowed; prototype trusts the physical USB connection, with explicit confirmation reserved for a product version.
- Allow multiple independently enrolled control devices. Authorization is one-to-many; active control remains one client at a time through the control lease.
- Use mDNS/DNS-SD for discovery only. Advertise endpoint identity and capabilities, never authorization secrets.
- Protect the board-hosted signaling API with HTTPS/WebSocket Secure (WSS). A self-signed certificate is sufficient for native clients when its certificate or public key is pinned during USB enrollment; a public CA is not required. Never accept arbitrary certificates.
- Bind authenticated signaling to client identity, endpoint identity, fresh nonces, protocol version, capabilities, and the WebRTC DTLS fingerprint. WebRTC encryption protects the channel but does not independently establish the correct application identity.
- Support per-client revoke, revoke-all, and physical factory reset. Identity changes must be visible and require deliberate re-enrollment.
- Rate-limit signaling handshakes and cap concurrent TLS sockets. Measure TLS handshake and long-lived WSS memory against internal heap on the exact ESP32-S3 module.

### Prototype decisions

- Direct USB connection alone authorizes prototype enrollment. Add a PIN or physical confirmation before treating this as a product.
- Client authorization-list administration, including per-client revoke and revoke-all, requires direct USB access. Enrolled LAN clients have no authority to administer other clients. Releasing a control lease is separate from revoking enrollment.
- Only the selected target has an active network control session. Do not keep WebRTC/signaling sessions warm for inactive targets; measure connection and switching time on the LAN.
- Lost client keys require USB re-enrollment. Factory reset is deliberate: hold `BOOT` while firmware is running; `RST` remains reboot-only. Keep a COM recovery command and verify both behaviors on the board.
- A takeover request waits 10 seconds. Explicit rejection preserves current control; no response grants control to the requester.
- First protocol encoding is CBOR. Control app decodes CBOR into readable diagnostics.
- First discovery path is mDNS/DNS-SD only. If discovery is unavailable, no target is shown; add Tailscale-based discovery later if needed.
- ESP32-S3 exposes `video: none` and relative mouse input only. The shared control app remains capability-driven so JetKVM can provide absolute mouse input when supported.
- First latency work establishes a measured baseline; fixed targets come after hardware and WebRTC results.
- Private travel-router LAN is the default development network. Phone hotspot is fallback. Captive-portal hotel Wi-Fi is unsupported for ESP32 onboarding.
- First threat model covers hostile LAN, not physical board compromise. Secure boot and flash encryption are deferred.

### Session security policy

- No public STUN/TURN or cloud signaling by default. Relay support is a later, explicit opt-in.
- Require authorization for all control actions, including future sleep-prevention, wake, and power operations.
- Show endpoint identity and local connection context so users can reject unexpected targets.
- Keep secrets out of logs; log connection state and device identifiers without authorization material.
- Fail closed on identity, protocol-version, or authorization mismatch.

Threats to cover in design and tests include LAN impersonation, stale authorization records, malicious signaling peers, replayed control messages, and accidental control of the wrong machine. Physical board compromise and protection of extracted firmware secrets remain outside the prototype threat model.

## Architecture

```text
                 +---------------------------+
                 | CLI / Windows tray client |
                 +-------------+-------------+
                               |
                 +-------------v-------------+
                 | Rust core                  |
                 | discovery, authorization, |
                 | sessions, hotkeys, HID    |
                 +------+------+--------------+
                        |      |
              +---------v-+  +-v----------------+
              | WebRTC     |  | device adapters  |
              | transport  |  | JetKVM / ESP32   |
              +------+-----+  +--------+---------+
                     |                  |
              +------v------+    +-----v---------+
              | DataChannels |    | endpoint HID  |
              | optional video|    | and protocol  |
              +-------------+    +----------------+
```

Keep transport separate from endpoint protocol. The shared session layer should handle discovery results, authorization, signaling, WebRTC lifecycle, reconnect state, input routing, control ownership, and target switching. Device adapters should translate endpoint-specific APIs or messages into that common model.

### Native Windows client

The primary desktop client is native Rust on Windows. It should expose a CLI first, with a tray application and global hotkey integration layered on later. Windows APIs should be used for keyboard/mouse capture, hotkeys, and sleep-prevention leases; platform-specific code should remain behind small interfaces so future Linux or macOS support does not leak into the core.

### WebRTC transport

Use WebRTC DataChannels for control and HID messages. Separate channels or message classes should distinguish control/session events from keyboard and mouse input. Video is optional and must not be required for an input-only endpoint.

Candidate transport shape:

- reliable ordered channel for authorization/session/control messages;
- low-latency input channel, with semantics chosen per message type rather than assuming every event needs retransmission;
- optional video track or channel negotiated only when supported and enabled.

CBOR is the selected message encoding. Channel reliability/ordering policy, HID report schema, and framing remain design decisions to freeze after endpoint constraints are known.

### Discovery and signaling

Use mDNS/DNS-SD for local discovery where supported. JetKVM hostname behavior and service advertisements must be verified instead of assumed to match the ESP32 service.

Discovery finds candidates. Each endpoint hosts a small local signaling service that exchanges the minimum metadata needed to establish a session: protocol version, device identity, capabilities, and WebRTC offer/answer/ICE information. Keep signaling local by default.

Use mDNS/DNS-SD metadata to advertise endpoint type, signaling port/path, authorization state, and capabilities such as `video: none`. The original plan identifies [`/webrtc/signaling/client`](https://github.com/jetkvm/kvm/blob/dev/web.go) as a JetKVM signaling reference. Treat that path and its behavior as unverified until adapter work begins. The ESP32 endpoint uses board-hosted signaling without copying JetKVM's application protocol.

### Identity and authorization

Separate three concepts:

1. Network discovery, which identifies a possible endpoint.
2. Authorization, which permits a client to control that endpoint.
3. Session establishment, which creates a fresh WebRTC connection for an authorized client.

Persist authorization state locally, support explicit revoke, and make identity changes visible. Avoid treating a hostname, IP address, or mDNS name as a device identity.

Each control device gets an independent credential. The ESP32-S3 stores authorized client public keys and supports individual revoke; it must not use one shared credential across all clients.

Enrollment, per-client revoke, and revoke-all are USB administration operations. LAN session authorization does not grant authorization-list administration privileges.

Authorization does not imply concurrent control. A device has one active control lease. Another authorized client may request takeover; the device revokes the old lease before granting the new one.

Control/session messages should cover lease ownership, takeover request/grant/revoke, heartbeats, and safe release of held keys and buttons.

### HID and switching

Keep the endpoint's USB HID connection attached independently of network-session lifetime. Maintain a network control session only to the selected target. A global hotkey changes the active target without requiring the user to reconfigure the physical keyboard or mouse. Switching releases or reconciles held keys/buttons, relinquishes the old lease, closes the old network session, and establishes a fresh authenticated WebRTC session to the new target before routing input. Inactive targets retain no warm network sessions.

Input routing should define behavior for:

- keyboard press/release and modifier state;
- mouse movement, buttons, and wheel, with absolute mode available only for endpoints that advertise coordinate support;
- focus/target changes;
- control lease ownership and explicit takeover;
- disconnect while keys or buttons are held;
- local emergency escape back to a safe target or disabled state.

## Endpoint scope

### ESP32-S3 HID prototype

Start with an HID-only endpoint. Available hardware is three ESP32-S3-N16R8 boards, each with two USB ports, as reported by the user. N16R8 is the declared 16 MB flash / 8 MB PSRAM configuration; verify detected capacity, usable internal heap, and USB controller/port mapping in M0 before committing to the full WebRTC stack.

The prototype should validate public LAN discovery, local authorization, board-hosted HTTPS/WSS signaling, WebRTC DataChannels, keyboard/mouse forwarding, single-controller takeover, persistent HID behavior, and reconnect handling without waiting for video support.

The first network security path should be HTTPS/WSS signaling with certificate pinning and per-client public-key authorization. USB enrollment is the trust bootstrap; mDNS only locates the endpoint. The prototype must work when the LAN is observable or hostile, subject to availability attacks such as client isolation or packet blocking.

### Test and validation mode

Test mode must use the same authentication, signaling, WebRTC transport, message encoding, and ESP32 decoder as live control. Only the event source changes.

- Transport test sends deterministic scripted keyboard and mouse events and does not emit HID. ESP32 returns sequence numbers, timestamps, queue depth, drop/reorder counts, and normalized report summaries.
- HID bench test sends the same scripted events and permits ESP32 HID output. A local observer reads only the ESP32 HID device, using Windows Raw Input device identity, to verify host receipt without suppressing the physical keyboard or mouse.
- Live mode captures physical input only after explicit arming and retains an emergency escape path.
- Report transport round-trip, ESP32 processing/queue, and same-host send-to-HID receipt latency separately. Report client-to-ESP32 one-way latency only with a documented clock-mapping method and uncertainty; timestamps from different clocks alone are insufficient. Do not claim screen or application reaction latency from HID delivery alone.

The ESP32-S3 firmware should keep its endpoint protocol small and capability-based. It advertises `video: none` and relative mouse support only. USB HID output, signaling, authorization, and WebRTC implementation should be isolated so endpoint changes do not require redesigning HID behavior.

### JetKVM integration

Treat JetKVM as a later adapter, not as the definition of the core protocol. Determine its supported local API, authentication behavior, mDNS/DNS-SD advertisements, input path, video path, and reconnect semantics after the ESP32 vertical slice works.

The desktop client must support video generally. JetKVM sessions should negotiate and display video when available; ESP32 sessions should advertise no video and omit the video UI. Reuse shared discovery, authorization, session ownership, hotkey, and power-policy layers where compatible.

JetKVM may use a different signaling or control protocol than the ESP32. Adapter boundaries must preserve the common user-facing model without pretending protocols are interchangeable. If JetKVM permits only one active session, expose that as the same exclusive-control model used by ESP32.

## Power and sleep behavior

Sleep prevention is optional and disabled by default for both the local Windows machine and a remote machine.

- Local sleep prevention requires an explicit user setting and should use a scoped Windows power request tied to an active session.
- Remote sleep prevention requires explicit endpoint capability, authorization, and user enablement. It must not be inferred from an active HID connection.
- Ending a session, disabling the option, or losing ownership should release the corresponding lease.
- On sleep/resume or network loss, reconnect using the persistent identity and revalidate authorization before restoring control.
- Wake-on-LAN and remote wake behavior are open questions, not first-milestone assumptions.

## Reconnect behavior

Treat reconnect as a normal session state, not an exceptional one.

1. Detect transport or endpoint loss.
2. Stop sending input and release local/remote held-input state safely.
3. Keep the selected target and authorization record unless the user revokes access.
4. Rediscover or resolve the endpoint locally if its address changed.
5. Re-establish signaling and WebRTC with fresh session data.
6. Revalidate identity, protocol version, capabilities, and authorization.
7. Restore input routing only after the session is authenticated, ready, and owns the control lease, provided live mode remains armed and the outage has not exceeded 60 seconds.

Use bounded backoff, clear UI/CLI state, and an explicit user escape path. Avoid reconnect loops that keep the machine awake or silently switch to another target.

Automatic recovery is limited to 60 seconds from detected connection loss, measured with elapsed time that includes Windows sleep. Only a fully authenticated, control-ready session completes recovery; partial signaling/transport progress does not restart the deadline. Discard input generated during the outage rather than queueing or replaying it. Held-input cleanup still applies.

If the outage exceeds 60 seconds, close pending connections, stop automatic retries, and disarm live input. Late connection completions must not resume control. Retain saved target information and enrollment. Rearming means explicitly selecting/connecting a target through the control app, including the same saved target; it does not require USB re-enrollment. Manual disconnect, disarm, or emergency escape cancels automatic recovery immediately.

## Working principles

- Keep LAN discovery, local authorization, and single-controller takeover as defaults.
- Build the smallest useful vertical slice: ESP32 discovery, authenticated DataChannel, USB HID, and reconnect.
- Keep transport, endpoint adapters, UI, and power policy separate.
- Treat optional video and sleep prevention as opt-in capabilities, not hidden side effects.
- Record protocol and security decisions in docs as implementation proceeds so the project remains easy to continue in Zed + Codex ACP.

## Design details still to resolve

Before implementing their dependent paths, record decisions for:

- Firmware language/toolchain, detected memory, USB controller topology for the available dual-USB N16R8 boards, and viable ESP32 WebRTC stack; these require the M0 hardware gate.
- Windows private-key storage, firmware key storage, certificate rotation/pinning updates, and authorization behavior after reboot.
- What factory reset erases, BOOT hold duration, and COM recovery protocol. Client administration is USB-only; enforce that boundary on recovery commands as well.
- Lease expiry and heartbeat timing, stale-session rejection, simultaneous takeover requests, and disconnect during the 10-second takeover window. Explicit rejection must continue to preserve current ownership.
- HID schema, rollover/modifier state, channel ordering/reliability, lost-event recovery, and emergency escape behavior.
- Protocol version/capability negotiation, authenticated signaling transcript, nonce/replay rules, error states, and test telemetry.

JetKVM API/authentication/video details, opt-in relays, wake/power support, portability, and publishing-name availability remain deferred until their milestones.
