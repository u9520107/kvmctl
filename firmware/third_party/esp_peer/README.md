# esp_peer

This source is a checked-in, source-built import from JetKVM Mini:

- Upstream: https://github.com/jetkvm/mini
- Upstream path: `third_party/esp_peer`
- Matching upstream base: `9683035eb3b5169c176a7025b98d317e2f6058ac`
- Local import commit: `7b9ac6b`
- Toolchain: ESP-IDF `6.1.1`, target `esp32s3`

The local import is 11 files different from the matching upstream base after
line endings are normalized (`+966/-6`):

Added local adapter and transport files:

- `src/core/jetkvm_ice_server.c`
- `src/core/peer_ice_reflexive.c`
- `src/core/peer_sctp_hid.c`
- `src/core/peer_sctp_hid_internal.h`
- `src/core/peer_sctp_receive.c`
- `src/core/peer_sctp_reconfig.c`
- `src/core/peer_sctp_streams.h`

Reviewed changes to imported files:

- `CMakeLists.txt`
- `src/core/sctp.c`
- `src/dtls_srtp_v6.c`
- `src/esp_peer.c`

The delta supplies JetKVM SCTP channel and receive-window integration, ICE
adapter hooks, DTLS fingerprint validation, fail-closed SRTP, UDP lifetime,
and memory-allocation behavior. Public headers are in `include/`; peer, ICE,
RTP, and SCTP internals are in `src/core/`. Normal builds use this checked-in
source directly; no prebuilt peer archive is accepted.

## Update procedure

1. Check out a pinned upstream commit into a staging directory. Do not update
   from a branch tip or edit this component in place.
2. Copy only the pinned `third_party/esp_peer` tree into a temporary worktree,
   normalize text to LF for review, and compare it with the recorded base:

   ```powershell
   git diff --no-index --ignore-space-at-eol --ignore-cr-at-eol `
     <upstream-base>/third_party/esp_peer firmware/third_party/esp_peer
   ```

   Record the changed-file list and line counts before accepting an update.
3. Stage the pinned component and the reviewed local delta together. Reapply
   local changes as a reviewed patch, not by silently overwriting files:

   ```powershell
   git add firmware/third_party/esp_peer
   git diff --cached --check
   ```

4. Preserve `LICENSE`, `LICENSES/`, upstream headers, and all copyright and
   SPDX notices. Update `THIRD_PARTY_NOTICES.md` when attribution or component
   versions change.
5. Gate adoption on the host regression command, a clean ESP32-S3 build with
   ESP-IDF 6.1.1, and board checks for ICE, DTLS fingerprint enforcement,
   channel open/reconnect, and HID behavior. Report any hardware check that
   cannot run. Do not flash as part of an import update.

## Separate upstream STUN review

Upstream commit
[`ae7fea063192296acfb722d5437543dc7ffdc4a7`](https://github.com/jetkvm/mini/commit/ae7fea063192296acfb722d5437543dc7ffdc4a7)
is a separate change and is not included in this import. It changes
`src/core/agent.c`, `src/core/stun.c`, and `src/core/stun.h`, plus an upstream
test, to bound parsing to received datagrams, reject oversized or truncated
attributes, clear refused messages, and keep invalid Data indications away from
handlers. Review and adopt it separately only after the host, S3, and board
gates above pass.

Dependencies: ESP-IDF, Mbed TLS, ESP-TLS, and `esp_libsrtp` `1.0.0`. The
project registers this folder through `EXTRA_COMPONENT_DIRS` and uses the
checked-in source override. Copyright and licensing notices remain in
`LICENSE` and `LICENSES/`.
