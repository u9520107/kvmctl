# kvmctl planning index

## Documents

- [Design](design.md): goals, architecture, trust model, identity, sessions, HID, endpoint scope, and unresolved design details.
- [Execution plan](execution-plan.md): current milestones, dependencies, and acceptance checks.
- [Test evidence](evidence.md): dated hardware observations, test results, and verification records.

Keep design decisions in the design document, status in the execution plan, and detailed evidence in the evidence record.

## Current position

M0 is closed as bootstrap and initial feasibility. M1 implementation is active; M1.1 closed on 2026-11-16 after user QA confirmed board selection/confirmation, hidden-SSID provisioning, LED operation, and removal of an unavailable SSID from the refreshed host list. M1.2 closed on 2026-11-17; M1.3 current-scope authorization and authenticated signaling acceptance closed on 2026-11-23. M2.2 resident tray and session lifecycle closed on 2026-11-25 after live tray and COM6 recovery evidence. M2.3 focused-session live input closed on 2026-11-28; comprehensive Windows/hardware acceptance continues in M2.4. Cross-user DPAPI verification and Surfshark-on mDNS are deferred to M2. Portable bridge-marker provisioning and full UI redesign are deferred. See the [execution plan](execution-plan.md#milestones) and [test evidence](evidence.md).

M1.2 closed on 2026-11-17. Board 2 exchanged known payloads over both DataChannels, reconnected cleanly, and ESP32/Windows receiving paths explicitly rejected mismatched identities with zero channel opens and application messages. Integrated heap and `webrtc_peer` task CPU were recorded; total SoC CPU and sustained-load behavior remain unmeasured. The unmodified `esp_peer` 1.5.3 backend remains prohibited; patched revision and tested firmware hashes are in [test evidence](evidence.md). Multicast discovery remains unresolved and belongs to later work.

M1.1 delivered LED-off firmware, the Rust USB client, Plug and Play discovery, editable Wi-Fi provisioning, and persisted RGB settings. User QA passed; unknown CH343 adapters still require explicit confirmation because portable bridge-marker provisioning is deferred. M1.2 transport tests still emit no HID reports and capture no physical input. Network-to-HID remains gated on M1.3 authorization and signaling.

## Confirmed decisions

- Hardware support targets the ESP32-S3 N16R8 configuration with native USB HID and a separate USB-UART administration path. Available DORHEA units are interchangeable development samples. No per-unit qualification or third-board test gates development. Discover ports and capabilities at runtime; do not hard-code sample numbers, serial numbers, MACs, COM ports, or IP addresses. Generate and enroll each device's cryptographic identity independently.
- Build the application and firmware in vertical slices. Add unit, certificate-fixture, and hardware integration tests alongside each feature. Security gates control when capabilities can be enabled, rather than whether unrelated implementation can begin.
- Onboard RGB LED control is an early M1.1 priority: default off, optional brightness/color through the local client, persisted across reboot. Status indications must respect the user's LED preference.
- Client administration requires direct USB access. Enrolled LAN clients cannot change the authorization list or revoke other clients.
- Native Rust resident tray and standalone CLI share core functionality, per-user settings, enrollment records, and escape bindings. CLI discovery, USB enrollment/management, and foreground KVM sessions require no tray app. No separate service or special process coordination is planned initially.
- Tray distinguishes no target, connected/local, and remote input, with visible non-ready states. Returning local keeps connection, ownership, and heartbeats. Short OSD notices identify connection/input changes.
- Resilience uses 3-second request/response heartbeats, three missed responses before client reconnect, and a 10-second endpoint lease safety expiry. Tray shows degraded quality without OSD noise. `wifi-test [COMx]` reports board-side Wi-Fi and gateway-ping quality. Live capture remains gated until lease-loss cleanup emits a real all-up HID snapshot.
- Enrolled boards appear by default; an optional filter exposes unenrolled discoveries. Only enrolled controllers can control or request takeover. A takeover prompt returns input local while retaining ownership pending the existing 10-second decision rule.
- Share multiple configurable escape bindings, with a usable default required before capture. Exact default awaits shortcut/keyboard testing. Tray binding switches local/remote; CLI escape disconnects and exits.
- Video uses a normal resizable/minimizable window independent of capture focus. Click-to-activate consumes its click; minimize returns local while retaining connection. Close releases input, disconnects, releases ownership, and clears selection; tray stays running, CLI session exits. Enrollment remains saved. Hide local cursor during capture only with verified remote-cursor visibility; restore on local/loss.
- Only the selected target has an active network control session. Switching closes the old session before establishing a new WebRTC connection with input local until explicitly activated; inactive targets have no warm sessions. Measure switching delay rather than assuming a latency bound.

- Reconnect automatically to the same selected target while an outage lasts no more than 60 seconds, revalidating identity, authorization, and ownership before restoring the connection. Keep input local, show OSD, and require explicit remote activation. Discard outage input; never replay it.
- If disconnected for more than 60 seconds, close pending connections, stop retries, and disarm. The user rearms through the control app by selecting/connecting a target again, including the same saved target. Saved target information and enrollment remain intact; USB re-enrollment is unnecessary.
- Manual disconnect, disarm, or emergency escape stops automatic recovery immediately. Initial live control always requires explicit arming.

## Questions needing user input

No outstanding user policy questions from this review. Engineering details and hardware verification remain below. Full agreed behavior is in the [control app interaction contract](design.md#control-app-interaction-contract).

## Review findings addressed

- M1 includes the minimal Windows enrollment/signaling/test client required to validate the ESP32 slice. M2 adds standalone CLI workflows, resident tray behavior, shared settings, OSD, and hotkeys.
- CBOR, USB enrollment, key-loss re-enrollment, 10-second takeover, relative mouse, and no ESP32 video remain settled decisions.
- Channel delivery semantics and HID message shapes are recorded in the design; add shared Rust/C vectors and parser rejection cases with their M1 codecs before those messages can affect control.
- JetKVM video belongs to M3 integration; M4 covers further video/power work.
- Physical board compromise stays outside prototype scope; hostile LAN and malicious peers remain in scope.
- Timing reports distinguish RTT and local durations from one-way measurements requiring clock mapping.

## Engineering work and deferred questions

M1 owns transport interoperability, certificate enforcement, USB identity/storage/reset, discovery, signaling, protocol vectors, and measured acceptance limits as part of the working product. M2 adds live capture and user-facing control flows only after their safety checks pass. M5 expands hardening coverage; it is not the first security milestone. A fault found on a sample is tracked as a device/recovery issue, not a dependency on qualifying an entire batch.

JetKVM API behavior, optional relay support, wake/power features, portability, and publishing-name checks stay deferred to their milestones.

Cross-user DPAPI verification and Surfshark-on mDNS discovery are M2 follow-ups. Current M1 use accepts physical re-enrollment plus known-IP routing when automatic discovery is unavailable.

A full visual redesign or migration from native Win32 controls to Slint/egui is deferred beyond M1.1. Keep M1.1 changes limited to usability fixes needed for acceptance.

The M1.1 RGB controls are a working prototype. In a later control-status slice, expose only LED enable/disable and brightness, then use red for disconnected, blue for connected/idle, and green while receiving control signals. Define other colors, blink patterns, and transitions with that implementation.
