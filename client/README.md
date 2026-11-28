# Windows USB client

This document covers original kvmctl Windows-client code and commands. Firmware
components, imported peer code, and probe examples retain separate notices in
the repository [third-party notices](../THIRD_PARTY_NOTICES.md).

See the [Windows prerequisites](../README.md#windows-client). From the workspace
root in Windows PowerShell, run `cargo run --locked --bin kvmctl-tray` to open
the native control window. Run `cargo run --locked -p kvmctl -- <command>` for
CLI commands. The client identifies supported USB-UART boards through Plug and Play,
shows firmware/network/RGB status, provisions Wi-Fi with a masked passphrase
field, and saves RGB settings. Credentials travel only over USB and are never
logged or returned.

CLI commands include `cargo run --locked -p kvmctl -- status [COMx]`,
`cargo run --locked -p kvmctl -- wifi [COMx]`,
`cargo run --locked -p kvmctl -- led [COMx] on|off <brightness> <RRGGBB>`,
and `cargo run --locked -p kvmctl -- ports`.

### M2.1 standalone CLI and saved target acceptance

Run these commands from Windows PowerShell in workspace root. Replace
`COM6`, `esp32-main`, and `192.168.50.137` with detected board, target name,
and board IPv4 address.

```powershell
cargo run --locked -p kvmctl -- ports
cargo run --locked -p kvmctl -- enroll COM6
cargo run --locked -p kvmctl -- clients COM6
cargo run --locked -p kvmctl -- target save COM6 esp32-main 192.168.50.137
cargo run --locked -p kvmctl -- targets
cargo run --locked -p kvmctl -- discover COM6
cargo run --locked -p kvmctl -- session esp32-main 192.168.50.137
```

Expected results: `ports` lists friendly Plug and Play CH343 board; `enroll`
reports this Windows user's DPAPI-backed client fingerprint; `clients` lists
it over USB; `target save` records endpoint identity and TLS pin under
`%LOCALAPPDATA%\kvmctl\targets.tsv`; `targets` shows saved record; `discover`
reports matching `_kvmctl._tcp.local` metadata when mDNS is available; and
`session` reports `Connected/local`, keeps physical-input capture off, waits
for Enter, releases lease, and reports `Session closed cleanly.`

  Diagnostics:

  ```powershell
  $env:KVMCTL_LOG = "debug"
  cargo run --offline -p kvmctl -- session m22-esp32 192.168.50.137
  Get-Content "$env:LOCALAPPDATA\kvmctl\kvmctl.log" -Wait
  cargo run --offline -p kvmctl -- telemetry COM6
  ```

  `KVMCTL_LOG` accepts `off`, `error`, `warn`, `info`, `debug`, or `trace`.
  Set `KVMCTL_LOG_FILE` to override the log path.

Use mDNS routing by omitting the IP:

```powershell
cargo run --locked -p kvmctl -- session esp32-main
```

If mDNS is unavailable, pass the current IPv4 address as shown above. Saved
target remains keyed by endpoint identity, not address, so address changes do
not create a new target. Saved last-known address is used only when discovery
returns no matching service.

Saved-target management and USB-only administration:

```powershell
cargo run --locked -p kvmctl -- target list
cargo run --locked -p kvmctl -- target remove esp32-main
cargo run --locked -p kvmctl -- clients COM6
cargo run --locked -p kvmctl -- revoke COM6 <client-fingerprint>
cargo run --locked -p kvmctl -- revoke COM6 all
```

`clients` and `revoke` open only selected USB Plug and Play board; they do not
use mDNS, saved target addresses, or foreground session. After revocation,
`session` fails until USB enrollment is performed again.

### Board 2 zero-HID takeover bench

Run from native Windows PowerShell using the board's CH343 port and LAN IPv4.
The second USB enrollment creates its own DPAPI-protected key under the current
Windows user. The regular client keeps its existing credential.

```powershell
cargo run --offline -p kvmctl -- enroll COM6
cargo run --offline -p kvmctl -- enroll COM6 takeover
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
  cargo run --offline -p kvmctl -- signaling-test COM6 takeover-request 192.168.50.137
```

  `takeover-request` uses the takeover identity only and waits up to 30 seconds
  for the active local owner to reject or accept the request. Use it while tray
  owns target.

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
