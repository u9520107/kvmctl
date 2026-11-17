# kvmctl planning index

## Documents

- [Design](design.md): goals, architecture, trust model, identity, sessions, HID, endpoint scope, and unresolved design details.
- [Execution plan](execution-plan.md): milestones, dependencies, hardware gates, acceptance checks, and evidence.

Keep design decisions in the design document and progress/evidence in the execution plan.

## Current position

M0 is closed as bootstrap and initial feasibility, following the 2026-11-16 planning revision. Start M1 product implementation now and verify security as each feature is built. Closing M0 does not mark outstanding security or resource tests passed; their owners and capability gates are in the [execution plan](execution-plan.md#milestones).

Evidence is sufficient to build: ESP-IDF and Rust toolchains work; representative N16R8 samples support CH343 UART administration alongside native HID, Wi-Fi association, and a basic same-board DataChannel exchange. Multicast discovery remains unresolved. The unmodified `esp_peer` 1.5.3 backend failed certificate identity checks and stays prohibited for secure control; the proposed patch has not passed its valid-session baseline. Rust interop tooling builds, but board-to-Rust and WSS integration remain to implement and test. Preserve these [findings](execution-plan.md#selected-backend-fingerprint-gate-failed-2026-11-16) while developing the real client and firmware.

Next deliverable is M1.1: LED-off firmware first, then a buildable product workspace and minimal Windows client that identifies compatible boards through USB Plug and Play, displays USB status, provisions Wi-Fi from an editable visible-SSID list with manual hidden-SSID entry, and controls supported RGB LED brightness/color. Users should not have to guess COM numbers, and the client must not probe arbitrary serial ports. M1.1 also includes portable USB identification: verify the bridge supports writable descriptors, provision a board-specific product string separately from ESP32 flashing, and verify recognition on a fresh PC plus descriptor persistence and driver compatibility. This provisioning remains pending; unknown CH343 adapters require explicit confirmation before opening, while previously confirmed mappings support automatic selection. Default the LED off and persist user preferences. Reuse useful code from `verification/m0` in product modules and attach tests to those modules. Keep unverified transport work in no-HID mode; independent client, USB, discovery, and storage work can proceed while certificate enforcement is fixed.

## Confirmed decisions

- Hardware support targets the ESP32-S3 N16R8 configuration with native USB HID and a separate USB-UART administration path. Available DORHEA units are interchangeable development samples. No per-unit qualification or third-board test gates development. Discover ports and capabilities at runtime; do not hard-code sample numbers, serial numbers, MACs, COM ports, or IP addresses. Generate and enroll each device's cryptographic identity independently.
- Build the application and firmware in vertical slices. Add unit, certificate-fixture, and hardware integration tests alongside each feature. Security gates control when capabilities can be enabled, rather than whether unrelated implementation can begin.
- Onboard RGB LED control is an early M1.1 priority: default off, optional brightness/color through the local client, persisted across reboot. Status indications must respect the user's LED preference.
- Client administration requires direct USB access. Enrolled LAN clients cannot change the authorization list or revoke other clients.
- Native Rust resident tray and standalone CLI share core functionality, per-user settings, enrollment records, and escape bindings. CLI discovery, USB enrollment/management, and foreground KVM sessions require no tray app. No separate service or special process coordination is planned initially.
- Tray distinguishes no target, connected/local, and remote input, with visible non-ready states. Returning local keeps connection, ownership, and heartbeats. Short OSD notices identify connection/input changes.
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
