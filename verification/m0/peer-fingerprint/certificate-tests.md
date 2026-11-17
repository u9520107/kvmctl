# Certificate verification test design

Purpose: repeatable verification of the project's own client and ESP32 firmware.
Use synthetic, explicitly test-only identities. No real enrollment keys,
network credentials, physical input, or HID reports belong in these tests.
This is the same identity-binding requirement as the existing bench, expressed
through named certificate fixtures and expected outcomes.

## Fixtures

- `certificate_a`: test key A and certificate A.
- `certificate_b`: distinct test key B and certificate B.
- `certificate_a_renewed`: another certificate for key A, with a new validity
  period and serial number.
- WSS-specific fixtures: expired certificate, incorrect DNS SAN, and an
  invalid handshake signature. Keep these separate from DTLS fingerprint cases.

All fixture private keys are public test data and must be labelled accordingly.
Generate fixtures with fixed metadata where possible; generate expiry cases
against a controlled test clock. Never use these keys for deployment.

## Unit checks

Check SHA-256 fingerprint parsing, exact digest matching, absent/duplicate/
conflicting attributes, invalid encoding, and connection-state cleanup.
Keep tests independent of the board and network. The existing extracted-C
parser checks cover parsing only; they do not establish TLS enforcement.

## Integration checks

First require a valid baseline for the exact implementation being tested.
Run the DTLS cases with the ESP32 acting as client and as server:

| Presented certificate | Expected SDP certificate fingerprint | Expected result |
| --- | --- | --- |
| A | SHA-256(DER A) | Channel opens; known payload exchange succeeds |
| B | SHA-256(DER B) | Channel opens; known payload exchange succeeds |
| B | SHA-256(DER A) | Certificate verification fails; no channel or application bytes |
| A renewed | SHA-256(DER A) | Certificate verification fails; DER certificate changed |
| Absent | SHA-256(DER A) | SDP/identity validation rejects; no channel or application bytes |

For WSS, pin SHA-256(DER SubjectPublicKeyInfo), not the whole certificate.
Certificate A renewed can pass the same SPKI pin if its SAN, validity,
structure, and handshake signature are valid. Certificate B must fail that pin.
Wrong SAN, expiry, and invalid signature must independently fail before
HTTP Upgrade or signaling data.

Use local test peers and USB signaling for the isolated DTLS bench, then repeat
through the real pinned/authenticated WSS path. Unit or desktop-only results
cannot replace integration evidence for the exact ESP32 binary.

Record fixture IDs, implementation/patch and firmware hashes, peer roles,
explicit verification result, channel-open count, application-byte count, and
resource measurements. A timeout, missing expected pin, or failed valid baseline
is inconclusive. Store no raw SDP credentials or deployed secrets in reports.

## Current status

Synthetic test-only certificates A, B, and renewed A are generated under
`fixtures/`; their DER and SPKI digests are recorded in `fixtures/manifest.json`.
The renewed A fixture uses the same key as A but has different DER. Parsing and
fingerprint extraction with the pinned Rust RTC certificate implementation
(`rtc` 0.21.1) passed, as did the Windows SDP fingerprint parser tests. These
are unit checks and do not prove a DTLS handshake.

The patched ESP32 backend's valid same-board probe baseline passed. Mutating
either side's SDP fingerprint produced explicit `pin_mismatch` failures as DTLS
client and server, with zero transient channel opens and zero received
application messages. The unmodified `esp_peer` 1.5.3 backend remains rejected.
Product firmware passed integrated Windows/ESP32 fixture A and B payload
baselines, clean reconnect, and active-session invalidation. Exact image,
patch, timing, and channel evidence is recorded in `docs/evidence.md`.
The integrated certificate matrix used product image SHA-256
`412361f2b331f854dd99890504e5921e7a7d13af9fdac55817f43218e135d65c` with the
same patched backend as the final telemetry image
`c66667cbae9a408d8cdd527420c543bfa401bb227741cc38f6cfe5068d8d3798`. Product
signaling roles were Rust offerer and ESP32 answerer; both receiving stacks
were exercised. The isolated backend matrix covered DTLS client and server.

| Product path | Case | Result |
| --- | --- | --- |
| ESP32 receives | A/A and B/B | Each matching fixture baseline exchanged 10 known payloads on both channels in initial and reconnect sessions. |
| ESP32 receives | B/A | Explicit fingerprint rejection before any DataChannel open or application echo. |
| ESP32 receives | Renewed A/old A | Explicit rejection despite the same public key; DER certificate changed. |
| ESP32 receives | Missing, malformed, duplicate identity | Each explicitly rejected before channel setup. |
| Windows receives | Product ESP certificate/expected fixture A | Explicit verifier mismatch; 0 DataChannels opened, 0 application messages sent. The product certificate matched in valid baselines. No fixture key was installed on hardware. |
| Windows receives | Missing, malformed, duplicate answer identity | Each rejected by strict client SDP parsing before remote description application. |

The Rust 0.21.1 verifier emits certificate mismatch as a warning while leaving
peer state `Connecting`. The client captures only warning/error classification,
not raw backend logs, and reports the explicit identity error before closing.
The first active session sends a conflicting fingerprint; the fresh reconnect
session sends malformed SDP. Both close, then reconnect opens cleanly.

The transport and certificate gate passed. The final telemetry image records
internal/PSRAM heap and `webrtc_peer` worker CPU; total SoC CPU and sustained
load remain unmeasured. See `docs/evidence.md`. Fixture material stays
test-only and must never be installed on a device or used for enrollment.
