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
| Absent | SHA-256(DER A) | Certificate verification fails |

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

Fixture-driven integration cases above are a design, not completed results.
The original backend's valid baseline passed, but wrong-fingerprint cases also
passed traffic, so it is rejected. The proposed patch builds and its parser
checks pass, but its valid board baseline has not passed. Original board app
was restored and verified after the latest run.
