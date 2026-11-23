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
