# kvmctl project plan

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
- Keep HID connections persistent while switching targets with a hotkey.
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
- Allow multiple independently enrolled control devices. Authorization is one-to-many; active control remains one client at a time through the existing control lease.
- Use mDNS/DNS-SD for discovery only. Advertise endpoint identity and capabilities, never authorization secrets.
- Protect the board-hosted signaling API with HTTPS/WebSocket Secure (WSS). A self-signed certificate is sufficient for native clients when its certificate or public key is pinned during USB enrollment; a public CA is not required. Never accept arbitrary certificates.
- Bind authenticated signaling to client identity, endpoint identity, fresh nonces, protocol version, capabilities, and the WebRTC DTLS fingerprint. WebRTC encryption protects the channel but does not independently establish the correct application identity.
- Support per-client revoke, revoke-all, and physical factory reset. Identity changes must be visible and require deliberate re-enrollment.
- Rate-limit signaling handshakes and cap concurrent TLS sockets. Measure TLS handshake and long-lived WSS memory against internal heap on the exact ESP32-S3 module.

### Prototype decisions

- Direct USB connection alone authorizes prototype enrollment. Add a PIN or physical confirmation before treating this as a product.
- Lost client keys require USB re-enrollment. Factory reset is deliberate: hold `BOOT` while firmware is running; `RST` remains reboot-only. Keep a COM recovery command and verify both behaviors on the board.
- A takeover request waits 10 seconds. Explicit rejection preserves current control; no response grants control to the requester.
- First protocol encoding is CBOR. Control app decodes CBOR into readable diagnostics.
- First discovery path is mDNS/DNS-SD only. If discovery is unavailable, no target is shown; add Tailscale-based discovery later if needed.
- ESP32-S3 exposes `video: none` and relative mouse input only. The shared control app remains capability-driven so JetKVM can provide absolute mouse input when supported.
- First latency work establishes a measured baseline; fixed targets come after hardware and WebRTC results.
- Private travel-router LAN is the default development network. Phone hotspot is fallback. Captive-portal hotel Wi-Fi is unsupported for ESP32 onboarding.
- First threat model covers hostile LAN, not physical board compromise. Secure boot and flash encryption are deferred.

- No public STUN/TURN or cloud signaling by default. Add relay support only as an explicit, later opt-in.
- Use authenticated signaling and encrypted WebRTC transport. Treat WebRTC encryption as transport protection, not as the complete identity model.
- Discovery is public; control is authorized. Show enough device identity and local connection context for the user to reject an unexpected endpoint.
- Store a persistent device identity and authorize clients independently. Identity changes must be deliberate, but authorization is not exclusive to one client.
- Allow multiple authorized clients, but grant control to only one active client at a time.
- Require authorization for control actions, especially sleep prevention, wake behavior, and any future power operations.
- Keep secrets out of logs. Log connection state and device identifiers without exposing authorization material.
- Fail closed when identity, protocol version, or authorization checks do not match.

Threats to cover in design and tests include LAN impersonation, stale authorization records, malicious signaling peers, replayed control messages, endpoint compromise, and accidental control of the wrong machine.

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

The exact channel policy, codec, and message framing remain implementation decisions after endpoint constraints are known.

### Discovery and signaling

Use mDNS/DNS-SD for local discovery where supported. JetKVM hostname behavior and service advertisements must be verified instead of assumed to match the ESP32 service.

Discovery finds candidates. Each endpoint hosts a small local signaling service that exchanges the minimum metadata needed to establish a session: protocol version, device identity, capabilities, and WebRTC offer/answer/ICE information. Keep signaling local by default.

Use mDNS/DNS-SD metadata to advertise endpoint type, signaling port/path, authorization state, and capabilities such as `video: none`. JetKVM currently exposes local WebRTC signaling at [`/webrtc/signaling/client`](https://github.com/jetkvm/kvm/blob/dev/web.go); the ESP32 endpoint should use the same broad model without copying JetKVM's application protocol.

### Identity and authorization

Separate three concepts:

1. Network discovery, which identifies a possible endpoint.
2. Authorization, which permits a client to control that endpoint.
3. Session establishment, which creates a fresh WebRTC connection for an authorized client.

Persist authorization state locally, support explicit revoke, and make identity changes visible. Avoid treating a hostname, IP address, or mDNS name as a device identity.

Each control device gets an independent credential. The ESP32-S3 stores authorized client public keys and supports individual revoke; it must not use one shared credential across all clients.

Authorization does not imply concurrent control. A device has one active control lease. Another authorized client may request takeover; the device revokes the old lease before granting the new one.

Control/session messages should cover lease ownership, takeover request/grant/revoke, heartbeats, and safe release of held keys and buttons.

### HID and switching

Maintain a persistent HID/session connection to the selected target where practical. A global hotkey changes the active target without requiring the user to reconfigure the physical keyboard or mouse. Switching must release or reconcile held keys/buttons so a target cannot retain stuck input.

Input routing should define behavior for:

- keyboard press/release and modifier state;
- mouse movement, buttons, and wheel, with absolute mode available only for endpoints that advertise coordinate support;
- focus/target changes;
- control lease ownership and explicit takeover;
- disconnect while keys or buttons are held;
- local emergency escape back to a safe target or disabled state.

## Endpoint plans

### ESP32-S3 HID prototype

Start with an HID-only endpoint. Target development boards have approximately 16 MB flash/ROM and 8 MB PSRAM/RAM; confirm the exact module and available internal heap before committing to the full WebRTC stack. A three-board prototype batch is sufficient for endpoint, client, and recovery testing.

The prototype should validate public LAN discovery, local authorization, board-hosted HTTPS/WSS signaling, WebRTC DataChannels, keyboard/mouse forwarding, single-controller takeover, persistent HID behavior, and reconnect handling without waiting for video support.

The first network security path should be HTTPS/WSS signaling with certificate pinning and per-client public-key authorization. USB enrollment is the trust bootstrap; mDNS only locates the endpoint. The prototype must work when the LAN is observable or hostile, subject to availability attacks such as client isolation or packet blocking.

### Test and validation mode

Test mode must use the same authentication, signaling, WebRTC transport, message encoding, and ESP32 decoder as live control. Only the event source changes.

- Transport test sends deterministic scripted keyboard and mouse events and does not emit HID. ESP32 returns sequence numbers, timestamps, queue depth, drop/reorder counts, and normalized report summaries.
- HID bench test sends the same scripted events and permits ESP32 HID output. A local observer reads only the ESP32 HID device, using Windows Raw Input device identity, to verify host receipt without suppressing the physical keyboard or mouse.
- Live mode captures physical input only after explicit arming and retains an emergency escape path.
- Report transport round-trip, client-to-ESP32 receipt, ESP32 processing/queue, and host HID receipt latency separately. Do not claim screen or application reaction latency from HID delivery alone.

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
7. Restore input routing only after the session is authenticated and ready.

Use bounded backoff, clear UI/CLI state, and an explicit user escape path. Avoid reconnect loops that keep the machine awake or silently switch to another target.

## Milestones

### M0: Decisions and skeleton

- Confirm repository layout and Rust workspace boundaries.
- Confirm exact ESP32-S3 board, USB topology, flash/PSRAM, internal heap, ESP-IDF version, and firmware/client language boundary.
- Document protocol, identity, capability, and error-state conventions.
- Define USB enrollment, per-client keys, certificate pinning, authenticated signaling, revoke/reset behavior, and one-controller lease semantics before LAN control work.
- Freeze the minimum wire protocol and test telemetry needed for scripted event comparison and latency measurement.
- Define the same-host test rig, including the ESP32 HID observer and acceptance criteria for keyboard, mouse, reconnect, and held-input cleanup.
- Choose WebRTC and mDNS/DNS-SD libraries after validating Windows and ESP32 constraints.
- Run the hardware and stack verification gate below. Record results in the plan before committing to the full vertical slice.

#### M0 hardware and stack verification gate

- Connect the board's `COM` and `USB` ports to the same Windows laptop. Confirm both enumerate, identify `COM` as USB-UART or USB-Serial/JTAG, and verify whether serial provisioning/logging remains usable while native USB runs HID.
- Confirm whether the two physical ports expose independent usable controllers or share a controller/PHY limitation. Verify native USB HID can remain attached to a target while `COM` handles enrollment and diagnostics.
- Provision Wi-Fi through `COM` before relying on network setup. Test a known 2.4 GHz private network; captive-portal hotel Wi-Fi is out of scope for board onboarding.
- Use a private travel-router LAN for repeatable development. Test laptop Ethernet plus ESP32 Wi-Fi, mDNS, HTTPS/WSS, and WebRTC with Surfshark enabled; repeat with local-LAN visibility settings changed only if needed.
- Build the smallest selected WebRTC proof on the exact board and firmware stack. Measure build size, internal heap, TLS/WSS handshake, DataChannel setup, CPU, reconnect, and sustained HID-path behavior.
- Record pass/fail results and update the architecture choices before M1 implementation.

### M1: ESP32-S3 vertical slice

- Advertise the prototype through mDNS/DNS-SD with endpoint type and capabilities.
- Implement local authorization and board-hosted HTTPS/WSS signaling.
- Establish authenticated WebRTC DataChannels.
- Forward keyboard and mouse events to USB HID.
- Add scripted transport/HID test modes, ESP32 telemetry, local HID observation, and latency reports.
- Enforce one active control lease and explicit takeover.
- Add disconnect, held-input cleanup, and reconnect tests.

### M2: Native Windows client

- Build `kvmctl` CLI discovery, authorize, connect, list, and switch commands.
- Add persistent identity storage and safe revoke behavior.
- Add Windows global hotkey and target switching.
- Add `kvmctld` only when a background process is needed for persistent behavior.

### M3: JetKVM adapter, later

- Capture JetKVM local protocol and service-advertisement behavior.
- Implement discovery and authorization integration.
- Map JetKVM input and optional video into the shared session model.
- Test JetKVM restart, network changes, and sleep/resume paths.

### M4: Optional video and power features

- Negotiate video only when endpoint and user settings allow it.
- Measure latency, bandwidth, CPU, and memory before optimizing.
- Add opt-in local sleep prevention.
- Add opt-in remote sleep prevention only after endpoint support and authorization are proven.

### M5: Hardening and portability

- Threat-model review, fuzz message parsing, and test downgrade/replay/identity failures.
- Package Windows client and firmware update/recovery paths.
- Evaluate Linux/macOS support without weakening Windows-first behavior.

## Risks and open questions

- **JetKVM compatibility:** What local APIs, service records, authentication, input APIs, and video interfaces are actually available when integration begins?
- **Signaling:** Which local signaling design works across Windows, JetKVM, and ESP32 without introducing a permanent server?
- **Authorization:** How should takeover and revoke behave after restart, including recovery after client-key loss?
- **ICE/NAT behavior:** Is host-only local connectivity sufficient, and when would an explicitly configured STUN/TURN relay be needed?
- **Identity storage:** Which Windows credential/storage mechanism is appropriate, and how should firmware keys be provisioned and revoked?
- **HID semantics:** How should relative/absolute mouse modes, modifiers, key rollover, and stuck-input recovery be represented?
- **Video:** Is optional video best implemented as a WebRTC media track, a device-native stream adapter, or deferred until after HID is stable?
- **Power:** Can JetKVM or the remote OS expose a safe sleep-prevention lease? Is wake-on-LAN in scope later?
- **Reconnect:** What state can each endpoint restore after sleep, reboot, or firmware restart?
- **Cross-platform scope:** Which parts belong in portable Rust core versus Windows-specific client code?
- **Naming:** `kvmctl` is preferred, but repository and executable availability should be checked before publishing.

## Working principles

- Keep LAN discovery, local authorization, and single-controller takeover as defaults.
- Build the smallest useful vertical slice: ESP32 discovery, authenticated DataChannel, USB HID, and reconnect.
- Keep transport, endpoint adapters, UI, and power policy separate.
- Treat optional video and sleep prevention as opt-in capabilities, not hidden side effects.
- Record protocol and security decisions in docs as implementation proceeds so the project remains easy to continue in Zed + Codex ACP.
