# Windows USB client

Build with `cargo build --release`, then run `target\release\kvmctl.exe` to open
the native control window. It selects a COM port, shows firmware/network/RGB
status, provisions Wi-Fi with a masked passphrase field, and saves RGB settings.
Credentials travel only over USB and are never logged or returned. CLI remains
available: `kvmctl status [COMx]`, `kvmctl wifi [COMx]`, `kvmctl led [COMx]
on|off <brightness> <RRGGBB>`, and `kvmctl ports`.
