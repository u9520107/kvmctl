# Windows USB client

From the workspace root, run `cargo run --offline -p kvmctl` to open the native
control window. It identifies supported USB-UART boards through Plug and Play,
shows firmware/network/RGB status, provisions Wi-Fi with a masked passphrase
field, and saves RGB settings. Credentials travel only over USB and are never
logged or returned.

CLI commands: `cargo run --offline -p kvmctl -- status [COMx]`,
`cargo run --offline -p kvmctl -- wifi [COMx]`,
`cargo run --offline -p kvmctl -- led [COMx] on|off <brightness> <RRGGBB>`,
and `cargo run --offline -p kvmctl -- ports`.
