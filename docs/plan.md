# kvmctl planning index

## Documents

- [Design](design.md): goals, architecture, trust model, identity, sessions, HID, endpoint scope, and unresolved design details.
- [Execution plan](execution-plan.md): milestones, dependencies, hardware gates, acceptance checks, and evidence.

Keep design decisions in the design document and progress/evidence in the execution plan.

## Current position

Private local Wi-Fi separate from the hotel network is set up, as reported on 2026-11-15. Board connectivity, multicast discovery, VPN behavior, HTTPS/WSS, and WebRTC remain unverified.

This round is paperwork. Implementation has not started. Next execution gate is M0 hardware/stack verification after required decisions are recorded.

## Confirmed decisions

- Hardware: three ESP32-S3-N16R8 boards, each with two USB ports, as reported by the user. Port/controller mapping and usable memory remain M0 measurements.
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
- Channel delivery semantics and HID schema remain open; encoding does not.
- JetKVM video belongs to M3 integration; M4 covers further video/power work.
- Physical board compromise stays outside prototype scope; hostile LAN and malicious peers remain in scope.
- Timing reports distinguish RTT and local durations from one-way measurements requiring clock mapping.

## Engineering work and deferred questions

M0 resolves firmware/toolchain and WebRTC feasibility, USB topology, key storage, certificate lifecycle, lease/heartbeat/race semantics, reset behavior, protocol schema, and measurable acceptance limits. These require design work or experiments rather than user guesses.

JetKVM API behavior, optional relay support, wake/power features, portability, and publishing-name checks stay deferred to their milestones.
