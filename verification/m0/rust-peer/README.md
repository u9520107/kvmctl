# M0 Rust / ESP32 transport bench

Windows `webrtc` 0.21.1 offerer, ESP32 answerer. Direct USB serial exchanges
hex-encoded SDP; DataChannel packets travel over the private LAN. No STUN,
WSS, product authorization, physical-input capture, or HID reports.

Build offline using the existing pinned source checkout and crate cache:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File verification/m0/rust-peer/build.ps1
cargo --config verification/m0/rust-peer/local-cargo.toml test --offline --manifest-path verification/m0/rust-peer/Cargo.toml
```

The script checks upstream commit `d658b5a803dd8dc389547450e245877310c9cbed`,
its recorded RTC submodule, and unchanged library source, then generates ignored
local Cargo configuration. `Cargo.lock` pins resolved
dependencies. It does not fetch missing crates.

After building/flashing the **hardened** serial-peer bench firmware, close
the Wi-Fi setup form's serial connection. Run with the ESP-IDF Python environment
(existing `pyserial` dependency):

```powershell
python verification/m0/rust-peer/bridge.py --port COM6 --bind 192.168.50.2 --messages 100
```

Bridge never resets the board. It sends `PEER STOP` before/after its run, then
`PEER OFFER <hex SDP>`. Board replies `M0 PEER SDP <hex SDP>`. One `control`
DataChannel sends sequenced binary messages and requires each exact echo before
the next send. Default is 100 messages; `--messages 1000` runs a longer sample.
Only the chosen IPv4 socket is bound. Local candidates must match that address;
remote candidates must be UDP host candidates within its private /24. This
intentional bench restriction is not a general network configuration interface.

Successful exit requires all echoes. RTT summary and redacted states are saved
under ignored `logs/`; raw SDP/ICE credentials are never written there.
This proves transport interoperability only. Secure product signaling,
enrollment, HID, and combined resource acceptance remain separate gates.
