# kvmctl execution plan

Status: M0 bootstrap closed; M1 implementation active; M1.1 closed on 2026-11-16. Portable bridge-marker provisioning and full UI redesign are deferred by user decision. Architecture and security requirements live in [design.md](design.md).

The milestone section is current scope. Dated test results and hardware observations live in [evidence.md](evidence.md). Earlier M0 gate results do not block M1. Board numbers identify samples only; no COM port, IP, MAC, or serial number is a product constant.

## Current status

- **M1.1: closed (2026-11-16).** User verified board selection/confirmation, hidden-SSID provisioning, LED controls, and that a disappeared network also disappears from the refreshed host SSID list.
- **M1.2: closed (2026-11-17).** Product ESP32/Windows payload exchange, both channel settings, reconnect, active-session invalidation, certificate rejection, and integrated heap/peer-task CPU sampling passed. Worker-task CPU is measured; total SoC CPU and sustained-load behavior remain unmeasured limitations. See [evidence.md](evidence.md).
- **M1.3: accepted for current scope (2026-11-23).** Recorded board checks cover enrollment/revocation, pinned WSS/WebRTC proofs, session isolation, resource recovery, and an on-device fail-once revoke persistence retry that remained revoked after reboot. With Surfshark disabled, source-built DNS-SD discovery and a full pinned signaling session succeeded. The normal-profile client enrolled and completed pinned manual-IPv4 signaling; known-IP routing is the accepted path when VPNs interfere with discovery. Cross-user DPAPI verification and Surfshark-on DNS-SD are explicitly deferred to M2. PC Ethernet remains the acceptance path; do not require the PC to switch to Wi-Fi. M1.2 DTLS certificate enforcement remains mandatory and unchanged. See [evidence.md](evidence.md).
- **M1: active.** Keep HID output and physical-input capture disabled until M1.4 ownership and scripted-HID safety checks pass.
- **Deferred by user:** CH343 bridge EEPROM marker provisioning and full UI redesign.
- **Deferred to M2 by user (2026-11-23):** Cross-user DPAPI verification. No second Windows account is available; revisit when multi-user testing becomes useful.
- **Deferred to M2 by user (2026-11-23):** DNS-SD discovery over Surfshark and similar VPNs. Physical re-enrollment plus known-IP routing is sufficient for current use; the VPN multicast cause is not resolved.

Product firmware in this slice has no LAN control or HID output. See [evidence.md](evidence.md) for test records and hardware observations.

## M0 preparation

- Separate durable requirements from implementation sequence.
- Resolve user policy questions in [plan.md](plan.md), or keep dependent work blocked.
- M0 protocol and security contracts are recorded in [design.md](design.md); network-stack measurements and cross-stack interop remain pending.
- Host inventory and dated bench evidence are in [evidence.md](evidence.md). No gate is passed without direct evidence from the board and private LAN.

## Milestones



### M0: Bootstrap and initial feasibility — closed

- Record the initial architecture, trust boundaries, candidate stack, and protocol direction.
- Establish working firmware/client toolchains and representative UART/HID coexistence, Wi-Fi, and basic DataChannel evidence.
- Carry unresolved findings into the implementation feature that owns them.

M0 exit is satisfied under the revised scope. This is a planning handoff, not a security acceptance. Further stand-alone feasibility work is justified only when it answers a specific implementation blocker.

### M1: Build the Windows client and ESP32 firmware together

Implement vertical slices in the product workspace. Reuse the existing bench modules where useful; avoid building a second complete test application before the product. Add tests and telemetry to the code being delivered. Independent work may proceed across slices while a gate is unresolved; a gate blocks its dependent capability, not all development.

#### M1.1: Workspace, USB setup, and device identity

- First firmware usability priority: turn the onboard RGB LED off at the earliest application initialization. Default to off, with no full-brightness startup animation; this must not wait for networking or security integration.
- Create firmware and Rust workspace boundaries plus a minimal usable Windows client that identifies compatible USB boards, displays status, and provisions local Wi-Fi. Use USB Plug and Play metadata rather than requiring users to guess a COM number; do not probe arbitrary serial ports.
- Portable bridge-marker provisioning is deferred by user decision on 2026-11-16 due EEPROM configuration risk. Keep VID/PID and bridge settings untouched; unknown CH343 adapters require explicit confirmation before opening. If resumed later, identify the fitted part, save full EEPROM configuration, alter only the product string, and verify driver binding and fresh-PC persistence.
- Provide an editable SSID dropdown populated from host-visible networks, while allowing manual entry for hidden SSIDs.
- Add simple USB/client LED controls: off/on, brightness, and RGB color where supported. Save the preference with device settings and restore it on reboot; connection/error status must respect off/brightness preferences. Use the supported hardware profile's LED wiring, not unit-specific identifiers.
- Discover compatible ports/capabilities and validate detected memory/partition bounds. Device identity comes from generated keys and USB enrollment, not sample labels or serial-number allowlists.
- Implement product USB framing, status/provisioning codecs, identity generation, and storage. Add shared C/Rust vectors and parser rejection tests as each command is implemented.
- Keep credentials local and redacted; test ordinary reboot/persistence alongside the storage feature. No LAN control or HID output is enabled by this slice.

Exit: the real client identifies compatible USB-UART candidates without requiring users to guess COM numbers, displays status, provisions Wi-Fi from visible SSIDs or manually entered hidden SSIDs, and controls supported RGB LED settings over USB. When metadata is ambiguous, show device/location details and require a choice before communication. Verify unrelated serial devices receive no kvmctl frames. Unknown CH343 candidates require explicit confirmation before opening. Verify LED off/default behavior, dim brightness, chosen color, and persistence across reboot/reconnect on representative hardware. Portable bridge-marker provisioning and fresh-PC marker recognition are deferred; this deliverable does not wait for complete WebRTC/WSS verification.

#### M1.2: Transport implementation and certificate binding

- Integrate ESP32-to-Rust DataChannels and known-payload echo into product transport modules, initially with no HID output. USB SDP exchange is a temporary internal test aid.
- Fix or replace the backend with the recorded fingerprint failure. Keep the unmodified failing backend excluded from secure control; do not weaken certificate verification to obtain a passing connection.
- Add synthetic certificate fixtures and unit/integration tests with the transport: valid A/A and B/B sessions connect, B/A is rejected in each DTLS role, and absent/malformed identities fail closed. A valid baseline must pass before rejection results count.
- Measure setup/reconnect behavior and initial heap/CPU during actual implementation. Build discovery support in parallel; defer service advertisement until a real endpoint exists in M1.3.

Exit: the integrated Windows/ESP32 transport exchanges known payloads, and its exact backend passes certificate identity tests. This gates use of that transport for remote input, not work on the client, USB, storage, discovery, or WSS modules.

#### M1.3: Enrollment, authenticated signaling, and discovery

- Implement USB enrollment and persistent independent client credentials; add USB-only list, per-client revoke, and revoke-all. Revoke active sessions immediately. Protect Windows private keys with DPAPI and verify persistence after app restart. Cross-user DPAPI verification is a deferred M2 follow-up.
- Implement pinned HTTPS/WSS with exact SPKI pin, required SAN, certificate validity, and signature verification. Keep M1.2 DTLS peer-certificate enforcement unchanged. Add WSS certificate fixtures for valid pin, different key, SAN, validity, and signature; failures must occur before HTTP Upgrade or credentials/signaling data.
- Implement signed challenge/transcript, exact protocol-version checks, fresh nonce/session replay rejection, and shared Rust/C vectors. Do not treat DataChannel open as authorization: application payload acceptance requires enrolled-client authorization, valid client proof, valid endpoint and client transcript proofs, and successful DTLS identity checks. Missing, invalid, or replayed proof must produce zero application payload acceptance or echo.
- Publish DNS-SD for the actual endpoint, consume it in the client, and expose discovery diagnostics. Treat discovery data as hints only. Test Ethernet compatibility; Surfshark-on DNS-SD is a deferred M2 follow-up because physical re-enrollment plus known-IP routing meets current use. A PC Wi-Fi-interface test is optional when PC Ethernet reaches the board's Wi-Fi LAN. Do not require switching the PC to Wi-Fi; discovery failure must not prevent USB administration.
- In the M1.3 signaling CLI, remember each USB-selected board's last IPv4 only after a successful pinned session. If DNS-SD finds no matching board, prompt before trying that address; an explicit IPv4 can skip browse. Both paths retain the USB-derived endpoint identity and TLS pin.
- Isolate signaling attempts. While client A has an active session, client B's unauthorized request, failed authentication, or competing offer must be rejected as `unauthorized` or `busy` without closing or changing A's session. Full ownership/takeover remains M1.4.
- Bound signaling resources in M1.3: handshake rate, concurrent TLS and WSS sockets, handshake deadlines, and partial-message buffers. Measure combined WSS/WebRTC internal heap and PSRAM on the ESP32-S3, then set and verify initial caps with enough headroom for USB administration and recovery. M1.4 owns broader sustained-load testing.
- Test unauthorized and revoked clients, persistence failures, identity changes, stalled/flooded handshakes, resource recovery, and continued USB administration alongside their flows. USB SDP exchange alone is not M1.3 acceptance.

Exit: an enrolled Windows client establishes an authorized, pinned session through real DNS-SD and HTTPS/WSS signaling on the ESP32. Negative identity, authorization, revocation, persistence, proof/replay, and competing-session tests pass; stalled/flooded attempts recover without displacing an active session or impairing USB administration. Combined WSS/WebRTC heap evidence establishes safe initial resource limits. M1.3 does not close without hardware/network evidence. Combined M1.2/M1.3 security gates and later M1.4 scripted HID/ownership gates must pass before network-originated input can affect USB HID.

#### M1.4: Scripted HID, ownership, and measured operation

- Add the event decoder, one-controller lease/takeover, and scripted transport/HID modes. Build the same-host HID observer and report schema with the events they measure.
- Before enabling network-to-HID output, pass M1.2/M1.3 plus implemented parser/ownership/held-input tests. Transport-test mode always emits zero HID reports; live physical-input capture remains off.
- Test cleanup on disconnect, timeout, takeover, revoke, and restart as each path is implemented. Use two enrolled client identities on one available board to test ownership; no particular pair of physical boards is required.
- Measure combined HID/Wi-Fi/WSS/WebRTC build size, minimum internal heap, PSRAM, CPU, setup, reconnect, and sustained traffic. Set repeat counts and acceptance limits from this integrated baseline.

Exit: one enrolled Windows client drives scripted HID through the real secure path, a second client demonstrates exclusive takeover, cleanup checks pass, and integrated resource/timing evidence is recorded.

#### M1.5: Recovery and repeatable bring-up

- Implement and test BOOT hold timing, reset erase scope, reboot-only RST, USB recovery, and key-loss re-enrollment with the actual identity/storage implementation.
- Define safe firmware/bootloader recovery and backup procedures. Track the existing sample bootloader SHA finding as a device issue; do not make unrelated client/firmware work wait for that unit.
- Make the same firmware/client bring-up flow work through detected capabilities and newly generated identity. Exercise that flow with the available device and fresh-identity test fixtures. Checks on another physical unit are optional sampling when available or when a hardware difference is suspected, never a third-board prerequisite.

M1 exit: the implemented vertical slice passes its security, scripted HID, ownership, recovery, and resource checks. M2 adds live capture, hotkeys, switching, and emergency escape with their own tests before live input is enabled.

### M2: Native Windows client

- Build standalone `kvmctl` CLI USB enumeration/enrollment, mDNS discovery, target management, and foreground KVM sessions without requiring the tray app.
- Revisit cross-user DPAPI validation when multi-user Windows testing is available.
- Revisit Surfshark-on DNS-SD only if VPN-routed automatic discovery becomes necessary; known-IP routing remains sufficient otherwise.
- Expose the M1 identity storage and USB-only administration/revoke behavior through complete CLI workflows.
- Build the resident Rust tray application with no-target, connected/local, and remote-control icons, plus visible connecting/recovery/unavailable/ownership states and target tooltips.
- Share Rust session functionality, per-user settings, enrollment records, and multiple escape bindings between modes. Choose and verify a broadly usable default; require a working escape binding before remote capture.
- Add Windows global hotkey, explicit remote activation, safe target switching, and short OSD notices. Local input retains the tray connection, ownership, and heartbeats; takeover prompts return local before interaction.
- Implement the [control app interaction contract](design.md#control-app-interaction-contract). CLI escape disconnects/exits; tray escape returns local while staying connected. Reconnect restores only the connection, with input local.
- No separate service, CLI-to-tray command broker, or special simultaneous-process detection is required initially. Prepare shared video-window behavior for video-capable adapters; verify actual video with M3 hardware.

M2 exit: standalone CLI and resident tray workflows, shared settings, status icons/OSD, ownership while local, arming, emergency escape, switching, revoke, and reconnect match the resolved design; no stuck input or unexpected routing to inactive targets.

### M3: JetKVM adapter, later

- Capture JetKVM local protocol and service-advertisement behavior.
- Implement discovery and authorization integration.
- Map JetKVM input and available video into the shared session model. JetKVM video belongs to this milestone; ESP32 remains input-only.
- Test JetKVM restart, network changes, and sleep/resume paths.
- With a JetKVM sample, verify standalone CLI sessions and normal resizable/minimizable video windows: click-to-activate consumes its click; minimize returns local without disconnecting; close releases input and ownership and ends the session. Verify cursor visibility before hiding the local cursor during capture.

### M4: Video refinement and opt-in power features

- Refine video negotiation/display when endpoint and user settings allow it; basic JetKVM video is delivered in M3.
- Measure latency, bandwidth, CPU, and memory before optimizing.
- Add opt-in local sleep prevention.
- Add opt-in remote sleep prevention only after endpoint support and authorization are proven.

### M5: Extended hardening and portability

- Expand the security coverage built through M1/M2: threat-model review, parser fuzzing, and wider downgrade/replay/identity regression cases. Security does not wait until M5.
- Package Windows client and firmware update/recovery paths.
- Evaluate Linux/macOS support without weakening Windows-first behavior.

## Acceptance checklist

Implement report schemas and repeatable tests with their M1 features; stabilize them as those slices close. Fixed latency targets follow the integrated measured baseline. Apply session/scripted/HID checks in M1; complete live capture, target-switching, and emergency-escape checks in M2 before enabling live input.

- **Enrollment/identity:** two clients enroll independently over direct USB; LAN discovery cannot enroll a client; an unexpected endpoint identity is rejected.
- **Authorization:** unregistered/revoked clients cannot obtain control. Enrolled LAN clients cannot mutate the authorization list; enrollment, per-client revoke, and revoke-all require direct USB. USB revocation of active control releases held input and invalidates its lease. Restart never silently restores revoked access.
- **M1.3 session isolation:** keep client A connected while client B is unauthorized, fails client or transcript authentication, or submits a competing offer. Reject B without closing A, changing A's authorization, or accepting B's application payload. Full ownership/takeover tests remain M1.4.
- **M1.3 application gate:** establish DTLS successfully, then omit, corrupt, or replay each transcript proof. In every negative case, record zero application payloads accepted and zero echoes before authorization. Verify app data opens only after client authorization, both transcript proofs, and DTLS identity checks pass.
- **M1.3 resource limits:** exercise handshake rate limiting, concurrent TLS/WSS caps, handshake deadlines, and maximum reassembled-message buffering. Stalled/flooded clients must time out or receive `rate_limited`/`resource_exhausted`, free resources, preserve any active session, and leave USB administration responsive. Record combined WSS/WebRTC internal heap and PSRAM minima before setting the initial caps; M1.4 owns sustained-load coverage.
- **M1.3 credentials/discovery:** verify two independent USB-enrolled client keys, DPAPI-protected Windows persistence, USB-only list/revoke/revoke-all and active-session invalidation; validate WSS SPKI/SAN/date/signature rejection before Upgrade; exercise shared challenge/transcript vectors, replay/version rejection, DNS-SD discovery diagnostics, and Ethernet/Wi-Fi. Cross-user DPAPI and Surfshark-on DNS-SD are deferred to M2.
- **Transport mode:** deterministic events use the same authentication, WSS signaling, WebRTC, CBOR, and ESP32 decoder as live mode; no USB HID is emitted. Compare sequence numbers, normalized reports, drop/reorder counts, and queue telemetry.
- **HID bench mode:** observer filters by ESP32 Raw Input device identity; expected key/modifier/button/wheel/relative-motion reports arrive without suppressing physical devices.
- **Ownership:** exactly one active control lease; old lease invalid before new owner emits input. Test explicit rejection and no-response takeover after 10 seconds. Implement the documented single-pending-request policy and its race tests together in M1.4.
- **Enrollment visibility:** normal lists default to enrolled boards; an optional filter shows unenrolled discovery results. Unenrolled clients cannot control or generate takeover prompts. Verify independent enrolled controller/board pairs cannot interfere through unauthorized requests.
- **Local/remote modes:** returning local in tray mode releases held input but retains connection, lease, and heartbeats. Takeover prompts first return local; rejection retains ownership without automatically resuming remote input. CLI management and foreground sessions work without a resident process; CLI escape disconnects and exits. Both modes use shared per-user settings and bindings; remote capture requires a usable escape binding.
- **Tray/OSD:** distinguish no target, connected/local, and remote input, plus non-ready connection/ownership conditions. Show target and routing changes through short OSD notices, including recovered-but-local. Explicit disconnect clears selection but preserves enrollment.
- **Video lifecycle (M3 hardware):** resizing/minimizing uses ordinary window controls. Minimize returns local and retains connection/lease; close releases held input, disconnects, and clears selection, leaving tray running or ending the CLI command. Activation clicks do not reach the target. Cursor restoration works on escape/loss; hiding depends on verified remote-cursor visibility. Input-only targets require no video window or focus.
- **Held-input safety:** switch, disconnect, timeout, revoke, endpoint restart, and emergency escape clear or reconcile held keys/buttons. Define heartbeat/lease expiry and maximum cleanup delay before implementation.
- **Selected-target sessions:** switching releases the old lease and closes its network session before connecting the new target with input local. Remote activation is explicit. Inactive targets retain no warm WebRTC/signaling sessions. USB HID remains attached. Measure new-session setup and switch latency on the LAN; do not assume it meets an unmeasured target.
- **Reconnect:** test endpoint restart, address change, network loss, and Windows sleep/resume. Revalidate identity, authorization, capabilities, and ownership before restoring the connection. Recovery leaves input local, restores the local cursor, and shows OSD; remote input requires explicit activation. Recover automatically to the same selected target only while the outage has not exceeded 60 seconds; discard outage input. Test recovery just before the deadline and failure to recover beyond it, including sleep lasting over one minute. Partial handshakes must not reset the deadline. After timeout, pending connections close, retries stop, input stays disarmed, and late completions cannot resume it. Explicit target selection/connection through the app rearms using saved enrollment. Manual disconnect/disarm/escape cancels recovery immediately. Never silently select another target.
- **Latency/resources:** record transport RTT, board-local processing/queue time, same-host send-to-HID receipt, CPU, flash, PSRAM, and internal-heap use during TLS/WSS setup and sustained traffic. Report one-way timing only with clock mapping and uncertainty.
- **Recovery:** verify deliberate BOOT factory reset, RST reboot-only behavior, COM recovery, and key-loss USB re-enrollment. Document agreed erase scope and recovery authorization.

M1.3 must establish initial handshake/socket/message caps and resource headroom from combined WSS/WebRTC measurements. M1.4 establishes repeat counts, cleanup deadlines, sustained-load limits, and HID-path limits from integrated behavior. M1 cannot close with its required fields unresolved; they are not prerequisites for starting it.

## Detailed evidence

Historical test results, current user acceptance, board observations, and supporting procedures are in [evidence.md](evidence.md).
