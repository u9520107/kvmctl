# kvmctl execution plan

Status: planning only. Architecture and security requirements live in [design.md](design.md); this document tracks order, gates, and evidence.

## Current evidence

| Item | Status | Evidence / next check |
| --- | --- | --- |
| Private local Wi-Fi separate from hotel network | Set up, user reported | Reported 2026-11-15; topology and isolation not independently tested |
| ESP32-S3 hardware and batch | User confirmed | Three ESP32-S3-N16R8 boards, two USB ports each; verify detected memory and port/controller mapping in M0 |
| COM/native USB topology | Not tested | M0 hardware gate |
| Board Wi-Fi provisioning | Not tested | Confirm 2.4 GHz compatibility; provision through COM |
| mDNS, VPN compatibility, HTTPS/WSS, WebRTC | Not tested | Validate on actual private LAN and board |
| Firmware/client implementation | Not started | Repository contains planning docs only |

Private LAN is the development environment. Hostile-LAN identity and authorization requirements remain unchanged.

## This paperwork round

- Separate durable requirements from implementation sequence.
- Resolve user policy questions in [plan.md](plan.md), or keep dependent work blocked.
- Prepare M0 decisions, test rig, and acceptance checklist.
- Hardware work and implementation follow this paperwork round. No gate is passed without evidence.

## Milestones

### M0: Paperwork, then hardware and stack verification

- Resolve user policy questions in [the planning index](plan.md).
- Confirm repository layout and Rust workspace boundaries on paper; create the skeleton when implementation starts.
- Use the three dual-USB ESP32-S3-N16R8 boards; verify USB topology, detected flash/PSRAM, internal heap, ESP-IDF version, and firmware/client language boundary.
- Document protocol, identity, capability, and error-state conventions.
- Define USB enrollment, per-client keys, certificate pinning, authenticated signaling, revoke/reset behavior, and one-controller lease semantics before LAN control work.
- Freeze the minimum wire protocol and test telemetry needed for scripted event comparison and latency measurement.
- Define the same-host test rig, including the ESP32 HID observer and acceptance criteria for keyboard, mouse, reconnect, and held-input cleanup. Specify clock mapping and measurement uncertainty.
- Choose WebRTC and mDNS/DNS-SD libraries after validating Windows and ESP32 constraints.
- Run the hardware and stack verification gate below. Record results in this execution plan before committing to the full vertical slice.

#### M0 hardware and stack verification gate

- Connect the board's `COM` and `USB` ports to the same Windows laptop. Confirm both enumerate, identify `COM` as USB-UART or USB-Serial/JTAG, and verify whether serial provisioning/logging remains usable while native USB runs HID.
- Confirm whether the two physical ports expose independent usable controllers or share a controller/PHY limitation. Verify native USB HID can remain attached to a target while `COM` handles enrollment and diagnostics.
- Provision Wi-Fi through `COM` before relying on network setup. Test a known 2.4 GHz private network; captive-portal hotel Wi-Fi is out of scope for board onboarding.
- Use a private travel-router LAN for repeatable development. Test laptop Ethernet plus ESP32 Wi-Fi, mDNS, HTTPS/WSS, and WebRTC with Surfshark enabled; repeat with local-LAN visibility settings changed only if needed.
- Build the smallest selected WebRTC proof on the exact board and firmware stack. Measure build size, internal heap, TLS/WSS handshake, DataChannel setup, CPU, reconnect, and sustained HID-path behavior.
- Record pass/fail results and update architecture choices before M1 implementation. If WebRTC cannot fit or USB enrollment cannot coexist with HID, stop and revise the design.

M0 exit: board and USB topology identified; toolchain/library versions recorded; enrollment/session contracts documented; smallest secure WebRTC proof measured on the actual board. A configured LAN alone does not pass this gate.

### M1: ESP32-S3 vertical slice

- Build the minimal Windows client for USB enrollment, discovery, authenticated signaling, scripted input, and HID observation. M1 must not depend on the polished M2 CLI.
- Advertise the prototype through mDNS/DNS-SD with endpoint type and capabilities.
- Implement local authorization and board-hosted HTTPS/WSS signaling.
- Persist device/client identities, enrolled-client records, and revocations; verify restart behavior.
- Establish authenticated WebRTC DataChannels.
- Forward keyboard and mouse events to USB HID.
- Add scripted transport/HID test modes, ESP32 telemetry, local HID observation, and latency reports.
- Enforce one active control lease and explicit takeover.
- Add disconnect, held-input cleanup, and reconnect tests.
- Verify unauthorized clients, wrong endpoint pins, stale/replayed session data, and revoked clients cannot control HID; transport-test mode must never emit HID.

M1 exit: one enrolled Windows client drives one ESP32 through the real secure path; a second enrolled client demonstrates exclusive takeover; transport and HID bench results are recorded. Authorization and held-input checks pass for implemented scripted/HID/session paths. Live capture, hotkey switching, and emergency escape close in M2 before live input is enabled.

### M2: Native Windows client

- Build `kvmctl` CLI discovery, authorize, connect, list, and switch commands.
- Expose the M1 identity storage and USB-only administration/revoke behavior through complete CLI workflows.
- Add Windows global hotkey and target switching.
- Add `kvmctld` only when a background process is needed for persistent behavior.

M2 exit: arming, emergency escape, switching, revoke, and reconnect match the resolved design; no stuck input or unexpected routing to inactive targets.

### M3: JetKVM adapter, later

- Capture JetKVM local protocol and service-advertisement behavior.
- Implement discovery and authorization integration.
- Map JetKVM input and available video into the shared session model. JetKVM video belongs to this milestone; ESP32 remains input-only.
- Test JetKVM restart, network changes, and sleep/resume paths.

### M4: Video refinement and opt-in power features

- Refine video negotiation/display when endpoint and user settings allow it; basic JetKVM video is delivered in M3.
- Measure latency, bandwidth, CPU, and memory before optimizing.
- Add opt-in local sleep prevention.
- Add opt-in remote sleep prevention only after endpoint support and authorization are proven.

### M5: Hardening and portability

- Threat-model review, fuzz message parsing, and test downgrade/replay/identity failures.
- Package Windows client and firmware update/recovery paths.
- Evaluate Linux/macOS support without weakening Windows-first behavior.


## Acceptance checklist

Freeze report schema and exact test scripts in M0. Fixed latency targets follow the initial measured baseline. Apply session/scripted/HID checks in M1; complete live capture, target-switching, and emergency-escape checks in M2 before enabling live input.

- **Enrollment/identity:** two clients enroll independently over direct USB; LAN discovery cannot enroll a client; an unexpected endpoint identity is rejected.
- **Authorization:** unregistered/revoked clients cannot obtain control. Enrolled LAN clients cannot mutate the authorization list; enrollment, per-client revoke, and revoke-all require direct USB. USB revocation of active control releases held input and invalidates its lease. Restart never silently restores revoked access.
- **Transport mode:** deterministic events use the same authentication, WSS signaling, WebRTC, CBOR, and ESP32 decoder as live mode; no USB HID is emitted. Compare sequence numbers, normalized reports, drop/reorder counts, and queue telemetry.
- **HID bench mode:** observer filters by ESP32 Raw Input device identity; expected key/modifier/button/wheel/relative-motion reports arrive without suppressing physical devices.
- **Ownership:** exactly one active control lease; old lease invalid before new owner emits input. Test explicit rejection and no-response takeover after 10 seconds. Resolve concurrent-request policy in M0 before testing races.
- **Held-input safety:** switch, disconnect, timeout, revoke, endpoint restart, and emergency escape clear or reconcile held keys/buttons. Define heartbeat/lease expiry and maximum cleanup delay before implementation.
- **Selected-target sessions:** switching releases the old lease and closes its network session before connecting the new target. Inactive targets retain no warm WebRTC/signaling sessions. USB HID remains attached. Measure new-session setup and switch latency on the LAN; do not assume it meets an unmeasured target.
- **Reconnect:** test endpoint restart, address change, network loss, and Windows sleep/resume. Revalidate identity, authorization, capabilities, and ownership before input resumes. Recover automatically to the same selected target only while the outage has not exceeded 60 seconds; discard outage input. Test recovery just before the deadline and failure to recover beyond it, including sleep lasting over one minute. Partial handshakes must not reset the deadline. After timeout, pending connections close, retries stop, input stays disarmed, and late completions cannot resume it. Explicit target selection/connection through the app rearms using saved enrollment. Manual disconnect/disarm/escape cancels recovery immediately. Never silently select another target.
- **Latency/resources:** record transport RTT, board-local processing/queue time, same-host send-to-HID receipt, CPU, flash, PSRAM, and internal-heap use during TLS/WSS setup and sustained traffic. Report one-way timing only with clock mapping and uncertainty.
- **Recovery:** verify deliberate BOOT factory reset, RST reboot-only behavior, COM recovery, and key-loss USB re-enrollment. Document agreed erase scope and recovery authorization.

M0 sets repeat counts, cleanup deadlines, resource headroom, and pass/fail limits where these depend on board/stack behavior. M1 cannot close with these fields unresolved.

## Evidence record for each execution gate

Record results here as work proceeds:

- Date, board/module, firmware/client revision, tool/library versions.
- USB wiring, target/client roles, OS, and network/VPN configuration.
- Commands or procedure, expected behavior, actual result, and pass/fail.
- Build size, minimum free internal heap, PSRAM use, CPU, connection/reconnect behavior, and timing method/uncertainty.
- Redacted log/report paths; never credentials or private keys.
- Failures, resulting design changes, and next gate allowed to proceed.

No M0 hardware or stack results recorded yet.
