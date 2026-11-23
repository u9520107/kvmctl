# kvmctl design

Status: evolving product design; implementation status is in the [planning index](plan.md), with hardware and test results in [evidence.md](evidence.md). The M0 probe source is in [verification/m0/usb-hid-uart](../verification/m0/usb-hid-uart).

## Summary

`kvmctl` is a local-first, low-latency KVM control system for discovering and switching between computers through hardware endpoints. It should provide one native Rust core for discovery, authorization, transport, input routing, and device management, with separate endpoint adapters for an ESP32-S3 implementation first and JetKVM later.

The name follows the existing `monitorctl` naming style. Planned components are:

- `kvmctl`: standalone CLI using reusable Rust client functionality; no resident app is required.
- `kvmctl-agent`: ESP32-S3 firmware endpoint.
- KVM Control: resident native Rust tray application using the same core. A separate background service is not required for the initial design.

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
- Bind authenticated signaling to client identity, endpoint identity, fresh nonces, protocol version, capabilities, and the WebRTC DTLS fingerprint. WebRTC encryption protects the channel but does not independently establish the correct application identity; peers compare fingerprints out of band as described by [RFC 8827](https://www.rfc-editor.org/rfc/rfc8827).
- Support per-client revoke, revoke-all, and physical factory reset. Identity changes must be visible and require deliberate re-enrollment.
- Rate-limit signaling handshakes and cap concurrent TLS sockets. Measure TLS handshake and long-lived WSS memory against internal heap on the exact ESP32-S3 module.

### Prototype decisions

- Direct USB connection alone authorizes prototype enrollment. Add a PIN or physical confirmation before treating this as a product.
- Client authorization-list administration, including per-client revoke and revoke-all, requires direct USB access. Enrolled LAN clients have no authority to administer other clients. Releasing a control lease is separate from revoking enrollment.
- Only the selected target has an active network control session. Do not keep WebRTC/signaling sessions warm for inactive targets; measure connection and switching time on the LAN.
- Lost client keys require USB re-enrollment. Factory reset is deliberate: hold `BOOT` while firmware is running; `RST` remains reboot-only. Keep a COM recovery command and verify both behaviors on the board.
- A takeover request holds the current owner for a 10-second window. Explicit rejection preserves current control; otherwise, grant the requester when the window expires. If the current owner's lease expires first, release held input and grant the still-connected pending requester immediately.
- First protocol encoding is CBOR. Control app decodes CBOR into readable diagnostics.
- LAN discovery uses mDNS/DNS-SD; USB enumeration supports enrollment and administration independently. Default target lists show enrolled boards; an optional filter can show discovered unenrolled boards. Discovery grants neither control nor permission to request takeover. Add Tailscale-based discovery later if needed.
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

The primary desktop client is native Rust on Windows, with resident tray and standalone CLI modes. Both use the same Rust core and per-user settings, enrollment records, and escape bindings. Neither mode requires the other to run. Windows APIs should be used for keyboard/mouse capture, hotkeys, and sleep-prevention leases; platform-specific code should remain behind small interfaces so future Linux or macOS support does not leak into the core.

### Control app interaction contract

These are agreed product requirements, not claims about the current USB setup client. Registration/enrollment, selected target, connection, lease ownership, and input destination are distinct state. A saved board can be offline; a connected owner can keep input local.

**USB setup and board identification (M1.1):** The Windows client identifies supported boards from USB Plug and Play metadata and the board's USB-UART interface before opening the administration port. Users should not have to guess from anonymous COM numbers, and the client must not send kvmctl frames to arbitrary serial ports to discover a board. Show a friendly board/device label; COM number is diagnostic detail, not board identity. If multiple candidates or incomplete identifiers make the board ambiguous, show available device and USB-location details and ask the user to choose before communicating. After a valid status response, associate the endpoint identity fingerprint with that PnP device for later identification.

**Portable USB identification (deferred):** Bridge EEPROM provisioning is deferred by user decision on 2026-11-16 because it can disrupt the separate CH343 USB-UART path. Do not write bridge configuration in M1.1. Keep current VID/PID and driver settings unchanged; unknown CH343 devices require explicit confirmation before opening. If resumed, confirm the fitted bridge supports writable descriptors, save its full current configuration, change only product string, then verify driver binding and descriptor persistence on a fresh PC after replug/reboot. USB marker remains discovery metadata, not cryptographic identity.

**Wi-Fi setup (M1.1):** Provide an editable SSID dropdown populated with networks visible to the host. Keep the field editable so users can type hidden or otherwise unlisted SSIDs; network scanning is a convenience and must not restrict manual entry. Keep the passphrase masked and local, and never log or return it.

**Resident tray mode** stays running without requiring a main window. Selecting a target connects with input local; entering remote mode is explicit. Returning local releases held remote input but retains the connection and control lease, including heartbeats, for frequent local/remote switching. Inactive targets have no warm sessions.

The tray must distinguish at least these primary states with different icon shapes, not color alone:

| State | Meaning |
| --- | --- |
| No target | No selected target; input local. |
| Connected, input local | Selected target connected and owned; keyboard/mouse control this computer. |
| Remote control | Keyboard/mouse routed to selected target. |

Connecting, recovering, unavailable, and ownership loss must be visibly distinguishable from a ready connection, for example through a badge and tooltip. Tooltip identifies the selected target and input destination. Saved enrollment alone must not produce a connected icon.

**Standalone CLI mode** enumerates USB boards for enrollment/administration, discovers LAN boards through mDNS, lists/manages targets, and connects directly for a foreground KVM session. It requires no tray process or icon. A video-capable target can open a normal video window from the CLI; ESP32 remains input-only. CLI escape ends the session and disconnects rather than keeping an idle connected/local session. No special tray-process detection, command broker, or cross-process coordination is planned initially; normal access and ownership errors suffice if both modes are used together. Command spelling remains an implementation detail.

**Shared escape settings:** require a usable escape binding before enabling remote capture. Ship a reasonable default so manual setup is not mandatory; allow multiple bindings, including an optional Copilot-key binding on compatible keyboards. Both modes read the same per-user configuration. In tray mode the switching binding returns remote input to local without disconnecting, and can activate remote control from a ready local session. In CLI mode escape releases input, disconnects, and ends the session. Recognized escape combinations are handled locally rather than forwarded to the target. Final default remains to be chosen after Windows/macOS/Linux shortcut and keyboard testing; Ctrl+Shift+F10 is only a candidate, not a settled cross-platform guarantee.

**Ownership and takeover:** connected/local retains exclusive ownership. Only authenticated, enrolled controllers can request takeover; unenrolled discovery cannot trigger a prompt or disturb another controller. A takeover notice returns the owner to local input before displaying an interactive prompt, while keeping its connection and lease pending the decision. The existing 10-second rejection/no-response policy applies even if input was already local. Rejection keeps ownership and leaves input local until explicitly activated again. Enrollment allows separate controller/board pairs on the same LAN without granting cross-pair control.

**Switching and disconnect:** selecting B while connected to A first returns input local, releases held input, relinquishes A's lease, and closes A's session before connecting B. B starts with input local and requires explicit activation. Explicit Disconnect clears the selected target and releases ownership; enrollment remains saved. Closing a video session window is also an intentional disconnect, without a confirmation dialog. In tray mode the tray remains running with no selected target; in CLI mode the session command exits. Input-only tray sessions use Disconnect; input-only CLI sessions can end through escape.

**OSD:** show short local on-screen notices for connection and input-destination changes, including target name, disconnect/loss, recovery, and local/remote transitions. Reconnect must announce that input remains local. Takeover needs an actionable prompt rather than only a transient notice.

**Video windows:** when supported and enabled, use a normal resizable/minimizable window; keep video updating while input is local. Remote input capture is independent of video focus and also works without video. While local, clicking the video content may activate remote mode; consume that activation click so it does not click the target. Window controls remain local controls. Minimizing video returns input local but keeps the connection and lease, including when minimization is triggered externally. Ordinary focus loss alone need not end remote mode. Closing video returns local, releases held input, disconnects, and releases ownership. Reopening requires a new connection. While remote capture is active, hide the local cursor when the remote cursor is visible in the video; restore it immediately on returning local or losing the connection. Verify remote-cursor visibility per target rather than assuming every video feed includes it.

JetKVM is intended to use this same interaction model, but discovery, authentication, ownership, video/cursor behavior, and standalone CLI compatibility require testing with a hardware sample.

### WebRTC transport

Use WebRTC DataChannels for control and HID messages. Separate channels or message classes should distinguish control/session events from keyboard and mouse input. Video is optional and must not be required for an input-only endpoint.

Protocol 1.0 uses two data channels with explicit delivery policy: reliable ordered `control` for session, ownership, keyboard, button, wheel, and release state; unordered `motion` with zero retransmissions for relative mouse deltas. ESP32 advertises no video channel. CBOR framing and message shapes are defined in [First wire protocol and ownership rules](#first-wire-protocol-and-ownership-rules).

### Discovery and signaling

Use mDNS/DNS-SD for local discovery where supported. JetKVM hostname behavior and service advertisements must be verified instead of assumed to match the ESP32 service.

Discovery finds candidates. Each endpoint hosts a small local signaling service that exchanges the minimum metadata needed to establish a session: protocol version, device identity, capabilities, and WebRTC offer/answer/ICE information. Keep signaling local by default.

Use mDNS/DNS-SD metadata to advertise endpoint type, protocol version, signing-key fingerprint, signaling port/path, and non-sensitive capabilities such as `video: none`. Do not advertise enrollment or authorization state. Treat all discovery data as untrusted hints. The original plan identifies [`/webrtc/signaling/client`](https://github.com/jetkvm/kvm/blob/dev/web.go) as a JetKVM signaling reference. Treat that path and its behavior as unverified until adapter work begins. The ESP32 endpoint uses board-hosted signaling without copying JetKVM's application protocol.

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

Keep the endpoint's USB HID connection attached independently of network-session lifetime. Maintain a network control session only to the selected target. The tray hotkey switches local/remote input without dropping that session or lease. Target switching releases or reconciles held keys/buttons, relinquishes the old lease, closes the old network session, and establishes a fresh authenticated WebRTC session to the new target with input local until explicitly activated. Inactive targets retain no warm network sessions. See the [interaction contract](#control-app-interaction-contract) for tray/CLI differences.

Input routing should define behavior for:

- keyboard press/release and modifier state;
- mouse movement, buttons, and wheel, with absolute mode available only for endpoints that advertise coordinate support;
- focus/target changes;
- control lease ownership and explicit takeover;
- disconnect while keys or buttons are held;
- local emergency escape back to a safe target or disabled state.

## Endpoint scope

### ESP32-S3 HID prototype

Start with an HID-only endpoint targeting ESP32-S3 N16R8 hardware: 16 MB flash, 8 MB PSRAM, native USB HID, and a separate USB-UART administration path. The available DORHEA dual-USB-C units are interchangeable development samples. Product support is defined by these capabilities, not a batch size, sample number, USB serial number, MAC, COM port, or IP address. Detect device resources and port capabilities during bring-up, generate a unique cryptographic identity on each device, and enroll it through the same client flow. Use representative hardware tests alongside implementation; no third-unit qualification is required. Alternate power wiring or USB topologies need investigation only if the product actually uses them.

Onboard RGB LED control is a first M1.1 usability feature. Set the addressable LED off at the earliest application initialization and keep it off by default; avoid full-brightness startup/status animations. The local client exposes simple off/on, brightness, and RGB color settings over USB when the hardware supports them. Persist those settings with device configuration, restore them on boot, and keep them stable across reconnects. Status/error indications must honor the user's off/brightness setting. Define LED wiring through the supported hardware profile, report unsupported controls clearly, and extend the USB setting codec/tests with this feature rather than relying on a specific sample's serial number.

**Future LED status behavior (deferred beyond M1.1):** Replace arbitrary color selection with enable/disable and brightness settings. Use the LED to show control state: red for disconnected, blue for connected but idle, and green while receiving control signals. Define additional colors, blink patterns, and exact transitions when control states are implemented. Keep the current M1.1 RGB controls as the working prototype until then.

The DORHEA N16R8 board manual labels its separate red lamp as `PWR` and describes it as indicating power delivery; it does not document a GPIO control. Treat that lamp as a hardware power indicator, outside software LED settings, unless a board schematic confirms a controllable driver.

M0 bench decision (2026-11-15): use a temporary visible Windows PowerShell form over the board's CH343 UART0 to enter Wi-Fi details locally and display connection status. The bench firmware starts station mode only after an explicit UART command, selects `WIFI_STORAGE_RAM`, and builds with `CONFIG_ESP_WIFI_NVS_ENABLED=n`; it does not persist credentials. Once an IP arrives, it initializes the Espressif mDNS responder with hostname `kvmctl-m0-probe`. This is only an association, IP, and hostname-discovery probe: it publishes no DNS-SD service, opens no signaling listener, and sends no HID reports. Its line-oriented UART commands and setup form are test tooling, not the product USB administration protocol specified below. This follows the ESP-IDF station initialization/event pattern, the documented RAM storage option, and the separate mDNS component model ([station scenarios](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-guides/wifi-driver/station-scenarios.html), [`esp_wifi_set_storage`](https://docs.espressif.com/projects/esp-idf/en/stable/esp32s3/api-reference/network/esp_wifi.html), [mDNS](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/protocols/mdns.html)). The setup path remains unverified on-device until credentials are entered locally; no credentials belong in chat or logs.

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
7. Restore the connection only after authentication, readiness, and control-lease checks, within the 60-second recovery deadline. Keep input local, restore the local cursor, and show an OSD notice; remote input never resumes automatically. Require explicit activation to route input remotely again.

Use bounded backoff, clear UI/CLI state, and an explicit user escape path. Avoid reconnect loops that keep the machine awake or silently switch to another target.

Automatic recovery is limited to 60 seconds from detected connection loss, measured with elapsed time that includes Windows sleep. Only a fully authenticated, control-ready session completes recovery; partial signaling/transport progress does not restart the deadline. Discard input generated during the outage rather than queueing or replaying it. Held-input cleanup still applies.

If the outage exceeds 60 seconds, close pending connections, stop automatic retries, and disarm live input. Late connection completions must not resume control. Retain saved target information and enrollment. Rearming means explicitly selecting/connecting a target through the control app, including the same saved target; it does not require USB re-enrollment. Manual disconnect, disarm, or emergency escape cancels automatic recovery immediately.

## Working principles

- Keep LAN discovery, local authorization, and single-controller takeover as defaults.
- Build the smallest useful vertical slice: ESP32 discovery, authenticated DataChannel, USB HID, and reconnect.
- Keep transport, endpoint adapters, UI, and power policy separate.
- Treat optional video and sleep prevention as opt-in capabilities, not hidden side effects.
- Record protocol and security decisions in docs as implementation proceeds so the project remains easy to continue in Zed + Codex ACP.

## Implementation contracts and stack candidates

These choices guide the first implementation target. Representative samples pass CH343 UART0 plus native-HID coexistence. M0 bootstrap is closed under the revised [execution plan](execution-plan.md#milestones); build M1 product modules now and verify each contract with its implementation. A failed check gates the dependent capability, not unrelated development. Keep unverified transport experiments in no-HID mode; remote input requires the certificate, authorization, parser, ownership, and cleanup gates for that path. Live physical-input capture additionally requires the M2 arming/escape/switching gates.

### Firmware and client stack

- Keep ESP-IDF C; reject the unmodified `esp_peer` 1.5.3 backend from `esp-webrtc-solution` tag `v1.3.0` for secure control. Board 2 testing with IDF v6.1.1 passed a two-peer DataChannel baseline, then accepted mismatched SDP fingerprints and exchanged payloads in both DTLS roles. See [runtime evidence](evidence.md#selected-backend-fingerprint-gate-failed-2026-11-16). Its peer implementation is a prebuilt archive, but the visible C wrapper and DTLS code can be forked to bind the authenticated SDP fingerprint to the actual peer certificate. Evaluate that explicitly identified patch separately; reject mismatches before DataChannel open or HID. Require valid baseline and negative tests before accepting a patched candidate. If that fails, choose a backend with an explicit verification path. Do not include media/video modules in the HID proof.
- Use native Rust on Windows with `webrtc-rs` `0.21.1` as the client proof. Its maintainers recommend the 0.21 line, which is still pre-1.0 and may break APIs between minor releases. Pin versions for the proof and treat real ESP32-S3 interoperability as a required gate.
- Use ESP-IDF `esp_https_server` and its HTTP server WebSocket support as the firmware WSS candidate; the framework exposes pre-handshake callbacks for authentication. Use `tokio-tungstenite` with Rustls as the Windows WSS candidate; it accepts a caller-supplied TLS connector. The client certificate verifier must enforce the enrolled SPKI pin, SAN, validity, certificate structure, and handshake signatures while skipping only public CA trust. Implement this verifier and its valid/different-key/SAN/expiry/signature fixture tests together in M1.3. This gates acceptance of signaling identities, not the start of M1.
- Rust firmware remains possible through Espressif's Xtensa toolchain and ESP-IDF bindings, but it adds C/FFI integration for the available ESP WebRTC stack. It is not the first proof path.
- Use Espressif's separately installed `mdns` ESP-IDF component and Rust `mdns-sd` for discovery candidates. Discovery is advisory and never grants access. Verify multicast behavior on the private Wi-Fi with Tailscale and Surfshark states recorded.
- These candidates have host compile and isolated board transport evidence, not a passed secure board stack gate. Results are tracked in [the execution plan](execution-plan.md); secure ESP32 interoperability and full-stack resources remain unverified.

Primary sources reviewed: [ESP WebRTC Solution v1.3.0](https://github.com/espressif/esp-webrtc-solution/releases/tag/v1.3.0), [v1.3.0 ESP peer demo](https://github.com/espressif/esp-webrtc-solution/tree/v1.3.0/components/esp_peer/examples/peer_demo), [v1.3.0 `esp_peer` channel API](https://github.com/espressif/esp-webrtc-solution/blob/v1.3.0/components/esp_peer/include/esp_peer.h), [Rust WebRTC v0.21.1](https://github.com/webrtc-rs/webrtc/tree/v0.21.1), [Rust on ESP toolchain](https://docs.espressif.com/projects/rust/book/getting-started/toolchain.html), [ESP-IDF mDNS component instructions](https://github.com/espressif/esp-idf/blob/master/docs/en/api-reference/protocols/mdns.rst), [Rust `mdns-sd` API](https://docs.rs/mdns-sd/latest/mdns_sd/), [ESP32-S3 USB Device guide](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/peripherals/usb_device.html), [ESP-TLS v6.1.1](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/protocols/esp_tls.html), [HTTPS Server v6.1.1](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/protocols/esp_https_server.html), [ESP-IDF HTTP Server WebSocket support](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/protocols/esp_http_server.html), [`tokio-tungstenite` TLS connector API](https://docs.rs/tokio-tungstenite/latest/tokio_tungstenite/fn.connect_async_tls_with_config.html), and [Rustls custom certificate verifier API](https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html).

### USB topology constraint

ESP32-S3 USB-OTG and USB-Serial-JTAG share one internal PHY and cannot operate simultaneously through it. Native USB HID uses USB-OTG; a separate USB-UART bridge carries administration and diagnostics while HID uses the internal PHY. Representative probes confirm UART0 data exchange over CH343 while native keyboard and mouse interfaces remain enumerated. Use this split for USB administration plus HID and validate supported port capabilities during product bring-up. Same-port CDC+HID, alternate power wiring, and exact PCB tracing are not prerequisites for implementing this split. An external PHY is needed only if USB-OTG and USB-Serial-JTAG must operate simultaneously. See Espressif's [ESP32-S3 USB Device guide](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/peripherals/usb_device.html); sample evidence is in [the execution plan](execution-plan.md).

### Identity, enrollment, and certificate lifecycle

- Use separate ECDSA P-256/SHA-256 signing keypairs for the endpoint identity and each client. Generate the endpoint identity once and retain it in ESP-IDF NVS. Store only client public keys on the endpoint. Store each Windows client's private key as a DPAPI-protected per-user file. The prototype threat model does not include physical flash extraction; flash encryption remains deferred.
- The first ESP32 client store supports up to 8 independent enrolled clients; a full store returns resource_exhausted.
- USB `status` returns the endpoint identity SPKI and its SHA-256 fingerprint, plus the TLS certificate SPKI pin. The client verifies the fingerprint from the supplied SPKI before trusting it, then transfers its own public key for enrollment. USB presence is the prototype authorization factor. LAN requests cannot enroll, list, revoke, or reset clients.
- Host HTTPS/WSS uses a self-signed certificate checked by the enrolled SPKI pin. Certificate renewal with the same TLS public key keeps the pin. A TLS key change requires explicit USB pin refresh/re-enrollment; no network pin-rotation statement exists in protocol 1.0. A device identity key change requires factory reset and client re-enrollment. Never offer a certificate-accept-anyway path.
- Endpoint identity, TLS key, client keys, and enrolled-client records survive ordinary reboot. USB revoke removes the selected client, invalidates all its active sessions and pending takeover requests, and releases its held input. Revoke-all does the same for every client. Persist the complete updated authorization list and verify the commit before returning success; if persistence fails, return `internal_error`, keep affected clients blocked in RAM, and do not report revocation success. A running-firmware BOOT hold of 10 seconds performs factory reset; reset erases all kvmctl-managed NVS, including Wi-Fi credentials, device identity/TLS keys, clients, and revocations. `RST` only reboots. Re-enrollment is required after reset. Implement and verify the hold timing and erase scope with the real storage/recovery feature in M1.5.
- Keep enrollment, per-client revoke, revoke-all, and recovery administration on the direct USB serial path. Network authorization grants control only.
- Frame USB serial administration as 115200 8-N-1 with a four-byte big-endian payload length followed by one deterministic-CBOR request or response, capped at 4096 bytes. Version 1.0 requests use `{0: major, 1: minor, 2: command, 3: request_id bstr16, 4: payload}`; responses echo version, command, and request ID, with key `4` as result/error code and successful result map at key `5`. Command `0x60 wifi_set` carries `{0: SSID UTF-8 tstr (1–32 bytes), 1: passphrase bstr (8–63 printable ASCII bytes or 64 hexadecimal bytes)}`; `0x61 status` carries an empty map and returns result keys `0` endpoint fingerprint, `1` TLS pin, `2` Wi-Fi state (`0` unconfigured, `1` connecting, `2` connected, `3` failed), `3` firmware version, `4` endpoint SPKI DER, and `5` LED state map `{0: supported bool, 1: enabled bool, 2: brightness uint, 3: RGB bstr3}`; `0x66 led_set` carries `{0: enabled bool, 1: brightness uint (0–100), 2: RGB bstr3}`. Wi-Fi credentials are entered locally, never logged or returned, and stored in device NVS for this prototype. Identity-key/settings-at-rest protections remain assigned to their implementation security milestones. Unsupported LED profiles return error `7`. USB commands also cover enrollment, client listing/revoke, and revoke-all. Accept an administration command only when it arrives through UART0/CH343; do not infer host COM-open state because UART does not report it.

### Authenticated signaling and session establishment

- The device advertises `_kvmctl._tcp.local` with protocol major version, HTTPS/WSS port, endpoint identity fingerprint, and non-sensitive capability hints. A key fingerprint is SHA-256 over DER SubjectPublicKeyInfo. Its SRV hostname is `kvmctl-<first 16 lowercase hex characters of the endpoint identity fingerprint>.local`; WSS uses port 443 and path `/v1/ws`. Do not advertise authorization records or secrets. Treat all TXT records and addresses as untrusted hints.
- WSS uses TLS 1.2 only with `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`; TLS 1.0/1.1/1.3, static RSA, CBC, compression, renegotiation, and insecure mode are prohibited for protocol 1.0. The M1 implementation must verify this cipher appears in the actual ESP-TLS supported-suite list and fail the gate if it does not. Espressif documents per-server TLS-version and cipher-suite controls in [ESP-TLS v6.1.1](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/protocols/esp_tls.html) and [HTTPS Server v6.1.1](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/protocols/esp_https_server.html).
- The self-signed server leaf uses a separate ECDSA P-256 TLS key, `serverAuth`, `digitalSignature`, and a DNS SAN equal to `kvmctl-<first 16 lowercase hex characters of the endpoint identity fingerprint>.local`. USB `status` transfers the endpoint identity SPKI and SHA-256 pin of the TLS certificate SPKI. The Windows client requires that SAN, a valid date under the Windows system clock, and an exact SPKI pin match. It skips general CA-chain trust only; it still verifies the certificate structure, hostname, validity, and TLS handshake signatures. There is no accept-any or fallback path. TLS key changes require USB pin refresh/re-enrollment.
- The client connects only to the selected endpoint and verifies its pinned TLS SPKI before sending credentials. The endpoint verifies a client signature over a fresh device challenge before accepting session setup.
- The endpoint sends its sorted capabilities in the signed `challenge` before SDP exchange, removing the dependency on the later `ready` message. After SDP exchange, both parties sign the same deterministic-CBOR transcript map: keys `0` version major, `1` minor, `2` session ID, `3` client fingerprint, `4` endpoint fingerprint, `5` client nonce, `6` device nonce, `7` the same sorted capability strings, `8` WSS SPKI SHA-256, `9` SHA-256 of exact offer UTF-8 bytes, `10` SHA-256 of exact answer UTF-8 bytes, `11` client DTLS fingerprint, and `12` endpoint DTLS fingerprint. Each DTLS fingerprint is the 32-byte SHA-256 digest of the corresponding certificate, parsed from exactly one applicable SDP `a=fingerprint:sha-256` attribute as specified by [RFC 8122](https://www.rfc-editor.org/rfc/rfc8122); reject missing, conflicting, malformed, or weaker fingerprints. Each WebRTC stack must verify the remote DTLS certificate against that SDP fingerprint, and the transcript must match the same values. Sign `kvmctl/transcript/v1 || deterministic-CBOR-bytes` with ECDSA P-256/SHA-256 once; encode signatures as DER. The client verifies the endpoint signature; the endpoint verifies the client signature. Do not open control or grant a lease until both transcript checks and WebRTC DTLS fingerprint checks succeed. Enforce and test this as part of M1.2/M1.3. A backend that fails remains excluded from secure control while independent product work proceeds.
- Require exact protocol version 1.0. Version 1.0 has no optional capability namespace: reject duplicate or unknown capability identifiers and extensions. Keep error codes stable and machine-readable; never include secrets in errors or logs.
- Do not use public STUN/TURN or cloud signaling in the prototype. ICE may use host candidates on the private LAN. Relay support remains a later opt-in.
- Treat DataChannel open as transport readiness only. Do not accept or echo application payload until enrolled-client authorization, client proof, both transcript proofs, and both DTLS peer-identity checks succeed for the same session. Missing, invalid, or replayed transcript proofs must leave accepted application bytes and echoes at zero.
- Reject unauthorized or competing signaling attempts as `unauthorized` or `busy` without closing or changing an existing authorized session. Full ownership and takeover behavior remains M1.4.
- Bound signaling handshake rate, concurrent TLS/WSS sockets, handshake and partial-message deadlines, and reassembled-message buffers in M1.3. Record combined WSS/WebRTC internal-heap and PSRAM minima before accepting initial caps; stalled or flooded attempts must recover while USB administration remains responsive. M1.4 covers broader sustained-load behavior.

### First wire protocol and ownership rules

- Encode messages with [RFC 8949 deterministic CBOR](https://www.rfc-editor.org/rfc/rfc8949): definite-length items, shortest integer encodings, unsigned-integer map keys in ascending order, and no tags or floats. Reject duplicate or unknown keys, unknown message types, text WebSocket messages, malformed values, and oversized messages. Use one CBOR map per reassembled WebSocket binary message or DataChannel message; WebSocket fragments are transport framing and are not parsed separately ([RFC 6455 §5.4](https://www.rfc-editor.org/rfc/rfc6455#section-5.4)). Cap each reassembled WSS message at 16 KiB; reject extensions and message compression.
- Network envelope map keys are fixed: `0` major version (`1`), `1` minor version (`0`), `2` message type (uint), `3` random 128-bit session ID (byte string, created by the client per attempt), `4` lease epoch (uint32; zero before a lease), `5` sequence (uint64, per direction and transport channel, starting at zero), and `6` type-specific payload (map). Require exact version 1.0 for the first implementation. Cap complete DataChannel control messages at 512 bytes and motion messages at 64 bytes; a maximum 32-key `keyboard_state` envelope is 274 bytes worst case. Cap each WSS message at 16 KiB and each SDP value at 8 KiB. WSS and ordered control channels require the next sequence exactly; unordered motion accepts only a sequence greater than the last applied, dropping and counting older/equal values.
- WSS type IDs are `0x01 hello`, `0x02 challenge`, `0x03 client_proof`, `0x04 sdp_offer`, `0x05 sdp_answer`, `0x06 ice_candidate`, `0x07 ice_end`, `0x08 transcript_proof`, `0x09 ready`, and `0x0A error`. The challenge carries the sorted capability array before `client_proof`; its signature covers the array. The device verifies `client_proof` before accepting SDP. Both transcript proofs are required before `ready`.
- DataChannel type IDs are `0x20 lease_request`, `0x21 takeover_request`, `0x22 takeover_response`, `0x23 lease_state`, `0x24 heartbeat`, `0x25 takeover_notice`, `0x26 error`, `0x27 lease_release`, `0x30 keyboard_state`, `0x31 mouse_buttons`, `0x32 wheel`, and `0x40 motion`. `control` is reliable and ordered; `motion` is unordered with `maxRetransmits=0`. Set `manual_ch_create=true`; use `ESP_PEER_DATA_CHANNEL_RELIABLE` with `ordered=true` for `control`, and `ESP_PEER_DATA_CHANNEL_PARTIAL_RELIABLE_RETX` with `ordered=false` and `max_retransmit_count=0` for `motion`. The API exposes these fields, but declaration alone does not prove runtime semantics. See the v1.3.0 `esp_peer` channel API. Malformed messages close only the sender's session; if that session owns the lease, invalidate that lease and release held input. A non-owner failure must never disturb another client's lease. Drop/count stale epochs and duplicate/out-of-order motion; never apply them.
- WSS directions are fixed: `hello` client→endpoint; `challenge` endpoint→client; `client_proof` client→endpoint; `sdp_offer` client→endpoint; `sdp_answer` endpoint→client; `ice_candidate`, `ice_end`, and `transcript_proof` either direction; `ready` endpoint→client; `error` either direction. Payload fields are fixed by type (all keys are unsigned integers): `hello {0: client_fp bstr32, 1: client_nonce bstr32}`; `challenge {0: device_fp bstr32, 1: device_nonce bstr32, 2: device_signature DER bstr, 3: sorted capability tstr array}`; `client_proof` and `transcript_proof {0: signature DER bstr}`; `sdp_offer` and `sdp_answer {0: UTF-8 SDP tstr}`; `ice_candidate {0: candidate tstr, 1: media ID tstr, 2: media-line index uint}`; `ice_end {}`; `ready {}`; `error {0: error-code tstr}`. The signed challenge map is `{0: session_id bstr16, 1: major uint, 2: minor uint, 3: client_fp bstr32, 4: device_fp bstr32, 5: client_nonce bstr32, 6: device_nonce bstr32, 7: sorted capability tstr array}`. Sign `kvmctl/challenge/v1 || challenge-CBOR` and `kvmctl/client-proof/v1 || challenge-CBOR` with ECDSA P-256/SHA-256 (hash each concatenated message once); encode signatures as DER. The device checks enrollment and signature before accepting SDP. `ready` carries no capabilities and is sent only after both transcript signatures, actual DTLS certificate fingerprint checks, and SCTP DataChannel setup succeed.
- DataChannel directions are fixed: `lease_request`, `lease_release`, takeover requests, `keyboard_state`, `mouse_buttons`, `wheel`, and `motion` go client→endpoint; `lease_state` and `takeover_notice` go endpoint→client; `takeover_response` goes current owner→endpoint and endpoint→requester as the result. `error` may go either direction. `lease_request`, `lease_release`, and `heartbeat` have empty payloads. `takeover_request {0: request_id bstr16}`; `takeover_notice {0: request_id bstr16, 1: requester_fp bstr32, 2: response_window_ms uint}`; `takeover_response {0: request_id bstr16, 1: accepted bool}`; `lease_state {0: owner_fp bstr32 or null, 1: state uint}` where state is 0 idle, 1 owned, or 2 takeover pending; `error {0: error-code tstr}`. One pending requester is allowed. The endpoint notifies the current owner and starts a 10-second response window. An explicit rejection preserves the current lease and denies the request. An explicit acceptance acknowledges the request but does not shorten the window. At 10 seconds, the endpoint invalidates the old lease, releases held input, and grants the pending requester; if the owner releases or its lease expires earlier, release input and grant the still-connected pending requester immediately. An owner sends `lease_release` on manual disconnect or target switch; endpoint clears HID state before replying with the resulting `lease_state`. The client must receive state showing it no longer owns the lease or observe the old session close before opening a new target session. Session/DataChannel close, heartbeat expiry, revoke, and endpoint restart also release held state locally. `keyboard_state {0: modifier_mask uint8, 1: sorted unique [usage_page, usage] uint16 pairs, maximum 32}` is a full state snapshot. `mouse_buttons {0: mask uint8}` is a full snapshot with bits 0–4 left, right, middle, back, forward and bits 5–7 zero. `wheel {0: vertical int16, 1: horizontal int16}` is a signed relative HID delta. `motion {0: dx int16, 1: dy int16}` is a relative HID delta (positive x right, positive y down). ESP32 advertises `keyboard`, `mouse.relative`, and `video.none`; absolute mouse and video are absent. Outage input is never queued or replayed.
- The endpoint releases all keys/buttons locally whenever a lease ends, is revoked, or changes owner; cleanup must not depend on receiving a network command. It increments the lease epoch whenever ownership changes or a lease is invalidated. A session may mutate HID state only while it owns the current epoch. A malformed message closes only its sender's session and invalidates/releases that session's lease if it owns one; stale messages from non-owners cannot release or alter the active owner's lease.
- USB administration keeps the existing 115200 8-N-1 transport: a four-byte big-endian payload length followed by one deterministic-CBOR map, maximum 4096 payload bytes. The request map uses keys `0` major, `1` minor, `2` command type, `3` 128-bit request ID, and `4` request payload. The response echoes `0`–`3`, puts status uint at key `4` (`0 ok`, `1 invalid_request`, `2 not_found`, `3 busy`, `4 unsupported_version`, `5 resource_exhausted`, `6 internal_error`), and may include command result at key `5`. Command IDs/payloads: `0x60 wifi_set {0: SSID UTF-8 tstr, 1: passphrase bstr}` (SSID 1–32 bytes; WPA2 passphrase 8–63 ASCII bytes or 64 hex digits); `0x61 status {}`; `0x62 enroll {0: client P-256 SPKI DER bstr}`; `0x63 list_clients {}`; `0x64 revoke_client {0: client fingerprint bstr32}`; `0x65 revoke_all {}`. `status` returns `{0: endpoint identity fingerprint bstr32, 1: TLS SPKI hash bstr32, 2: Wi-Fi state uint, 3: firmware version tstr, 4: endpoint identity SPKI DER bstr}`; clients verify key `0` equals SHA-256 of DER at key `4`. State is 0 unconfigured, 1 connecting, 2 connected, or 3 failed. `enroll` returns the client fingerprint; `list_clients` returns an array of fingerprints; other successful command responses have an empty result map. Successful `wifi_set` means credentials were accepted and stored, not that association succeeded. Never log or echo the Wi-Fi passphrase. USB presence is the prototype administration boundary.
- Example deterministic heartbeat encoding (version 1.0, session ID `00..0f`, epoch 7, sequence 0): `a7 00 01 01 00 02 18 24 03 50 00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f 04 07 05 00 06 a0`. This is a document vector only; implement shared Rust/C vectors with the M1 heartbeat codec before it can affect lease state.
- Encode ECDSA/SHA-256 signatures as strict ASN.1 DER byte strings over the specified transcript bytes. Reject unsupported versions and invalid sequence progression on ordered channels. Drop and count stale lease epochs and out-of-order motion without letting stale/non-owner messages change the active lease. The current owner keeps control during the 10-second takeover window; explicit rejection preserves its lease. No response grants control only after the old lease is invalidated and held input is released. On owner expiry, grant the still-connected pending requester immediately; otherwise clear the request. A reconnect always uses fresh signaling, DTLS, authorization, and lease checks.
- Send a lease heartbeat every 1 second. Expire the lease after 3 seconds without a valid heartbeat, increment the lease epoch, and release all keys/buttons. Serialize lease changes on the endpoint and permit one pending takeover; reject further requests as `busy`. Treat heartbeat and takeover timing as prototype values to measure under the actual LAN and VPN conditions.
- Use stable error codes at minimum: `unsupported_version`, `unauthorized`, `pin_mismatch`, `replay`, `busy`, `lease_expired`, `invalid_capability`, `rate_limited`, and `resource_exhausted`.

### Test telemetry contract

Scripted transport mode uses the same USB-enrolled identity, pinned WSS, signed transcript, WebRTC DataChannels, CBOR parser, and event decoder as live control, but the firmware must not emit USB HID. Include event sequence, endpoint monotonic timestamp, receive queue depth, drop/reorder counts, normalized key/button state, and lease epoch. HID bench mode adds a host Raw Input observer filtered to the selected endpoint's current USB device identity, not a fixed sample serial. Report board-local processing and same-host send-to-HID receipt separately. Report one-way timing only after defining clock mapping and uncertainty; otherwise report RTT and local durations. Implement this telemetry with M1.4; live physical-input capture stays disabled until M2's live-input gates pass.
The USB transport-status result is the bounded telemetry snapshot for scripted mode: last event sequence, board timestamp, lease epoch, response queue depth, event/rejection/drop/reorder counters, and normalized held key/button flags. Older status responses may omit these optional fields; clients treat omitted telemetry as zero.

## Design details still to resolve

Resolve and verify these items alongside the owning implementation slice. Existing contracts guide implementation; unresolved measurements do not delay unrelated modules:

- M1.1: validate detected memory and supported UART/native-USB capabilities during bring-up. Investigate schematic differences only when an observed compatibility issue requires it; no batch or third-board gate.
- M1.1: resolve Windows Plug and Play identification for supported USB-UART boards, including multiple or ambiguous candidates, without probing arbitrary serial ports. Verify automatic identification when unique and a clear user choice when ambiguous. Include bridge-marker provisioning and fresh-PC recognition, with descriptor persistence and Windows driver compatibility; this remains pending.
- M1.1: populate the editable SSID dropdown from host-visible networks and verify manual entry remains available for hidden SSIDs.
- M1.2: integrate the selected ESP-IDF backend with the pinned Windows `webrtc-rs` client. Resolve the known unmodified backend failure through a verified patch or replacement while building product transport modules.
- M1.2/M1.3: add certificate fixture unit/integration tests to the actual DTLS and WSS implementations. Matching fixtures must establish a valid baseline; mismatched DTLS certificate identity in either role must fail before DataChannel open, with zero application bytes/HID reports. Wrong WSS SPKI, SAN, validity, or handshake signature must fail before HTTP Upgrade/signaling ([RFC 6455 §4.1](https://www.rfc-editor.org/rfc/rfc6455#section-4.1)). Follow [RFC 8122 §5](https://www.rfc-editor.org/rfc/rfc8122#section-5) and [RFC 8827 §9.1](https://www.rfc-editor.org/rfc/rfc8827#section-9.1); test the exact deployed ESP32 binary, not just a desktop parser. These gate the dependent session capability, not the start of product implementation.
- M1.1–M1.4: implement shared Rust/C vectors and rejection cases with each USB, WSS, DataChannel, and lease codec before its messages affect state. The envelope, type IDs, fields, and heartbeat example above are the starting contract.
- M1.5: implement observable reset/recovery confirmation and verify hold timing/erase scope with the real storage model. Preserve the sample bootloader finding as a device issue.
- M1.4: measure integrated build size, TLS/WSS/DataChannel setup, internal heap, PSRAM, CPU, reconnect, and sustained HID-path traffic; set repeat counts, headroom, cleanup deadlines, and resource limits from that baseline.
- M2: implement and test emergency escape, arming, capture, and switching together before enabling live physical input.

JetKVM API/authentication/video details, opt-in relays, wake/power support, portability, and publishing-name availability remain deferred until their milestones.
