# M0 ESP32 DTLS fingerprint probe

Tests the selected ESP32-S3 backend before investing in Windows signaling code.
Two peers run on separate cores of one board. No media, HID, physical input,
NVS, external ICE servers, or product signaling is used. An isolated SoftAP
provides the local interface; its fresh random password is never printed.
No client needs to join it.

Prerequisites: installed ESP-IDF 6.1.1 and the `esp-webrtc-solution` checkout at
tag `v1.3.0` (its `esp_peer` component declares version `1.5.3`). The existing
upstream demo's cached `esp_libsrtp` 1.0.0 is reused to avoid registry access.
These paths are bench prerequisites, not vendored dependencies.
The build script checks source revision `3971094e2e7c2ab7cb4de59e4eae710e1902d2b4`
and rejects changes to the peer implementation.

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File verification/m0/peer-fingerprint/build.ps1
# Optional: -WebrtcSource <path-to-esp-webrtc-solution>
C:/Espressif/tools/python/v6.1.1/venv/Scripts/python.exe verification/m0/peer-fingerprint/run.py --port COM6
# For a repeat run: --backup <previous-factory-app-backup.bin>
# Reuse first verifies the supplied backup matches the current app in flash.
```

Close the local setup form's serial connection first. The runner checks the
recorded Board 2 partition layout, backs up the complete factory-app slot,
flashes/verifies only the test app at `0x10000`, captures UART, and restores
and verifies the complete backup in `finally`. Bootloader, partition table,
NVS, and other data partitions are never written. If restoration itself fails,
retain the printed backup path and recover over COM; do not erase the board.
Do not use `idf.py flash` or the generated full-image flash command.

Reset clears the prior probe's RAM-only Wi-Fi credentials. After restoration,
enter credentials again through the local setup form if further LAN checks
are needed. Never put credentials in shell arguments or chat.

Each case observes 20 seconds. Baseline must exchange the known payload in
both directions. Negative cases alter every SHA-256 fingerprint in the SDP
from peer 0, then peer 1, leaving the actual certificates unchanged. A startup
self-check verifies mutation preserves SDP syntax. Per-case counters retain
transient channel openings and payload receipt even if the channel closes.

- `PASS` for baseline: bidirectional known payload received, no harness errors.
- `SECURITY_FAIL`: mutated SDP leads to a channel connection/open or payload
  receipt, with mutation confirmed and no signaling/validation errors.
- `INCONCLUSIVE`: baseline failure, mutation failure, signaling error, timeout,
  or generic connection rejection. Generic failure does not prove fingerprint
  enforcement. Positive security acceptance requires DTLS-specific rejection
  evidence and confirmed DTLS role coverage beyond these counters.

Runner exits nonzero on failed or inconclusive suites, after attempting restore.
Logs/backups stay in ignored `logs/<timestamp>/`; ICE password/ufrag lines are
redacted. Archive these locally when recording evidence. This checks only the
backend; it does not establish ESP32/Rust interoperability, WSS authentication,
HID coexistence, full-stack resources, or M0 completion.
