# kvmctl

Native Windows client and ESP32-S3 firmware for a KVM controller. The client
uses the board's USB-UART port for administration and Wi-Fi setup. See the
[planning index](docs/plan.md) for architecture, milestones, and test evidence.

Original kvmctl code is MIT-licensed. Imported firmware keeps its upstream
notices in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Host regression

From the repository root in an x64 Visual Studio Developer PowerShell:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-host.ps1
```

The same command runs all seven firmware host checks, generates its live-input
fixture under `firmware/build/`, runs the ignored Rust fixture handoff test,
and runs the locked Rust workspace suite. On Windows, use an x64 Visual Studio
Developer PowerShell so missing `cl.exe`/`link.exe` is reported before checks
start. It never builds or flashes firmware and never runs hardware checks.

## Windows client

In a Windows checkout, install stable Rust with the `x86_64-pc-windows-msvc`
toolchain and Visual Studio Build Tools with **Desktop development with C++**
and a Windows SDK. [Rust's MSVC target guide](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html)

From the repository root in PowerShell:

```powershell
cargo check --workspace --tests --locked
cargo run --locked --bin kvmctl-tray
```

For USB administration, connect the board's USB-UART port and confirm Windows
assigns it a COM port. See the [client README](client/README.md) for commands.

Live input is enabled by default with the current Windows capture backend;
input remains local until the configured shortcut is pressed. See the
[host regression and interactive checks](docs/evidence.md#m23-live-input-review-verification-2026-11-27).

Live input uses three fixed channels: reliable ordered `control` for lease,
heartbeat, and session transitions; reliable ordered `input` for keyboard,
button, and wheel state; and unordered `motion` for relative mouse deltas.
The former `input_route` setting is no longer used; legacy records are ignored
and disappear when settings are rewritten.

Motion cadence defaults to 125 Hz. Change it without recompiling:

```powershell
cargo run --locked --offline -p kvmctl -- settings motion-rate set 125
```

Use `settings motion-rate list` to inspect the active value and `reset` to
restore 125 Hz.

Keyboard remapping is deferred. On keyboards whose Copilot key emits
`Win+Shift+F23`, bind the physical key as a kvmctl escape shortcut:

```powershell
cargo run --locked -p kvmctl -- settings escape add COPILOT
```

## Portable Windows package

From a Windows PowerShell with Rust, MSVC, and the Windows SDK installed:

```powershell
.\scripts\package.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1
```

`package.ps1` creates a versioned portable package containing the CLI and
tray binaries under `dist`. `install.ps1`
installs the newest complete package into `%USERPROFILE%\tools\kvmctl`, adds
that directory to the user `PATH`, creates a login-startup shortcut, and starts
the client. Use `-NoStart`, `-NoStartup`, or `-InstallDirectory <path>` to
change those defaults. Installation does not build source code.

## ESP32-S3 firmware

Build from Windows PowerShell with ESP-IDF 6.1.1 for `esp32s3`, the pinned
`esp-webrtc-solution` checkout, and `esp_libsrtp` 1.0.0. See the
[Windows firmware build](firmware/README.md#windows-build-primary) for setup
and commands. Board checks and app-only flashing are documented there too.
