# Windows USB client

See the [Windows prerequisites](../README.md#windows-client). From the workspace
root in Windows PowerShell, run `cargo run --locked -p kvmctl` to open the native
control window. It identifies supported USB-UART boards through Plug and Play,
shows firmware/network/RGB status, provisions Wi-Fi with a masked passphrase
field, and saves RGB settings. Credentials travel only over USB and are never
logged or returned.

CLI commands include `cargo run --locked -p kvmctl -- status [COMx]`,
`cargo run --locked -p kvmctl -- wifi [COMx]`,
`cargo run --locked -p kvmctl -- led [COMx] on|off <brightness> <RRGGBB>`,
and `cargo run --locked -p kvmctl -- ports`.

### Board 2 zero-HID takeover bench

Run from native Windows PowerShell using the board's CH343 port and LAN IPv4.
The second USB enrollment creates its own DPAPI-protected key under the current
Windows user. The regular client keeps its existing credential.

```powershell
cargo run --offline -p kvmctl -- enroll COM6
cargo run --offline -p kvmctl -- enroll COM6 takeover
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
```

The test keeps both pinned WSS/WebRTC sessions open. It checks explicit rejection,
heartbeat-backed timeout, stale input rejection, held-state cleanup, and final
lease release against the board. The test prints endpoint responses and epochs.

### Scripted USB HID bench

Flash the opt-in app using the [firmware bench workflow](../firmware/README.md#scripted-usb-hid-bench).
Connect Board 2's native USB port to the same Windows host as COM6.

```powershell
cargo run --offline -p kvmctl -- signaling-test COM6 hid-bench 192.168.50.137
```

The observer requires a matching USB keyboard and mouse under the serial
derived from the COM6 endpoint fingerprint. Keep the scratch window in focus and
the pointer inside it for the short script. The command checks Shift+A,
left-click, both wheel axes, relative motion, and releases on lease cleanup.
`scripted`, `takeover`, and `transport-test` continue to check zero HID output.
