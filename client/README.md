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

The test keeps both pinned WSS/WebRTC sessions open. On a HID-capable image it
also proves A's two neutral reports arrive before B's grant response, then
checks B input and final release. On a zero-HID image it requires the HID count
to stay unchanged.

Cleanup paths use one enrolled identity and verify physical neutral reports:

```powershell
cargo run --offline -p kvmctl -- signaling-test COM6 cleanup-disconnect 192.168.50.137
cargo run --offline -p kvmctl -- signaling-test COM6 cleanup-expiry 192.168.50.137
cargo run --offline -p kvmctl -- signaling-test COM6 cleanup-revoke 192.168.50.137
```

`cleanup-transfer-failure` is for an image built with
`KVMCTL_HID_BENCH_FAIL_TRANSFER_ONCE=1`; it must end disarmed with no grant.
Use `takeover-zero` on a HID-capable image when repeating the zero-HID takeover
path; it must leave the HID report count unchanged.

### Scripted USB HID bench

Flash the opt-in app using the [firmware bench workflow](../firmware/README.md#scripted-usb-hid-bench).
Connect Board 2's native USB port to the same Windows host as COM6.

```powershell
cargo run --offline -p kvmctl -- signaling-test COM6 hid-bench 192.168.50.137
```

Sustained HID-path load uses 100 repeats over 10,000 ms:

```powershell
cargo run --offline -p kvmctl -- signaling-test COM6 hid-sustain 192.168.50.137
```

The observer requires a matching USB keyboard and mouse under the serial
derived from the COM6 endpoint fingerprint. Keep the scratch window in focus and
the pointer inside it for the short script. The command checks Shift+A,
left-click, both wheel axes, relative motion, and releases on lease cleanup.
`scripted`, `takeover`, and `transport-test` check zero HID output when run on a
zero-HID image; on a HID-capable image, `takeover` is the physical ordering
bench described above.
