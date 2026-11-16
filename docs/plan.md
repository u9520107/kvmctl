# kvmctl planning index

## Documents

- [Design](design.md): goals, architecture, trust model, identity, sessions, HID, endpoint scope, and unresolved design details.
- [Execution plan](execution-plan.md): milestones, dependencies, hardware gates, acceptance checks, and evidence.

Keep design decisions in the design document and progress/evidence in the execution plan.

## Current position

Private local Wi-Fi separate from the hotel network is set up, as reported on 2026-11-15. The boards are DORHEA ESP32-S3-DevKit C N16R8 (Amazon ASIN B0CKXJLP4B, three-pack); the photo and listing confirm dual USB-C and a CH343P UART port. Boards 1 and 2 report ESP32-S3 rev 0.2, 16 MB flash, and 8 MB PSRAM. Both boards passed the CH343 UART0 + native HID enumeration subgate with the no-report probe. Board 2 now runs a repository M0 Wi-Fi/mDNS bench image; a local Windows setup form has been used once; Board 2 previously disconnected with ESP-IDF reason 201 (`WIFI_REASON_NO_AP_FOUND`); a 2026-11-16 board-side scan found the exact visible SSID on channel 2 at -24 dBm with WPA2, but a later valid-length dummy-password attempt reached the WPA four-way handshake and failed with reason 15, matching the expected wrong-password result; the locally entered real credentials now connect Board 2 at `192.168.50.137` with `mdns=on`; host hostname/multicast discovery remains unresolved. The user confirms a phone connects to the visible 2.4 GHz SSID with the same password. Board 1 remains on the prior no-Wi-Fi probe. Board 2's RST reboot returned to the probe, but ROM reported a SHA mismatch in the existing second-stage bootloader; its cause is unknown and the reset/recovery gate remains partial. Board 3 remains sealed and untested. Surfshark is connected; route evidence is recorded in the execution plan. The upstream ESP32-S3 peer demo compiles with ESP-IDF v6.1.1, and `webrtc-rs` passes a Rust-to-Rust loopback DataChannel check. The opaque ESP32-S3 `esp_peer` archive's SDP-to-DTLS fingerprint enforcement remains unproved, so it is not accepted for secure control yet. Remaining: resolve USB routing details, use the local form for Wi-Fi association and mDNS/VPN checks, complete pinned/authenticated signaling and ESP32-to-Rust WebRTC, measure full-stack resources, and close the bootloader/recovery finding.

M0 hardware/stack verification started 2026-11-15. Host evidence and the two-board no-report USB HID + CH343 UART coexistence subtest are recorded in [the execution plan](execution-plan.md). Product implementation has not started; bench firmware is in `verification/m0/usb-hid-uart` and the temporary local setup form is `verification/m0/wifi-setup.ps1`. Board/network and secure-interoperability gates remain open; do not start M1 until M0 findings are resolved.

## Confirmed decisions

- Hardware: three DORHEA ESP32-S3-DevKit C N16R8 boards, each with two USB-C ports, per the user's product listing. Boards 1 and 2 confirm CH343 UART data transfer concurrent with native HID; board 3 remains sealed and untested. Remaining USB routing details stay in M0.
- Client administration requires direct USB access. Enrolled LAN clients cannot change the authorization list or revoke other clients.
- Only the selected target has an active network control session. Switching closes the old session and establishes a new WebRTC connection; inactive targets have no warm sessions. Measure switching delay rather than assuming a latency bound.

- Reconnect automatically to the same selected target while an outage lasts no more than 60 seconds, revalidating identity, authorization, and ownership before resuming input. Discard outage input; never replay it.
- If disconnected for more than 60 seconds, close pending connections, stop retries, and disarm. The user rearms through the control app by selecting/connecting a target again, including the same saved target. Saved target information and enrollment remain intact; USB re-enrollment is unnecessary.
- Manual disconnect, disarm, or emergency escape stops automatic recovery immediately. Initial live control always requires explicit arming.

## Questions needing user input

No outstanding user policy questions from this review. Engineering details and hardware verification remain below.

## Review findings addressed

- M1 includes the minimal Windows enrollment/signaling/test client required to validate the ESP32 slice. M2 adds complete CLI workflows and hotkeys.
- CBOR, USB enrollment, key-loss re-enrollment, 10-second takeover, relative mouse, and no ESP32 video remain settled decisions.
- Channel delivery semantics and HID message shapes are recorded in the design; shared Rust/C vectors and parser rejection cases remain required before M1.
- JetKVM video belongs to M3 integration; M4 covers further video/power work.
- Physical board compromise stays outside prototype scope; hostile LAN and malicious peers remain in scope.
- Timing reports distinguish RTT and local durations from one-way measurements requiring clock mapping.

## Engineering work and deferred questions

M0 resolves firmware/toolchain and WebRTC feasibility, USB topology, key storage, certificate lifecycle, lease/heartbeat/race semantics, reset behavior, protocol schema, and measurable acceptance limits. These require design work or experiments rather than user guesses.

JetKVM API behavior, optional relay support, wake/power features, portability, and publishing-name checks stay deferred to their milestones.
