# kvmctl

Native Windows client and ESP32-S3 firmware for a KVM controller. The client
uses the board’s USB-UART port for administration and Wi-Fi setup. See the
[planning index](docs/plan.md) for architecture, milestones, and test evidence.

## WSL development

Use WSL 2 for ESP32-S3 firmware builds and host checks. Keep the checkout
under your Linux home directory for Linux tooling. [Microsoft’s WSL setup guide](https://learn.microsoft.com/en-us/windows/wsl/setup/environment)

On Ubuntu, install the host-check dependencies:

```sh
sudo apt update
sudo apt install git python3 build-essential clang binutils
```

From the repository root:

```sh
python3 firmware/test_usb_admin.py
python3 firmware/test_signaling_guards.py
```

Rust is optional in WSL for editing or formatting client code. Use Windows
for client builds, tests, and COM-port access. See the
[firmware host checks](firmware/README.md#host-side-firmware-guard-test).

## Windows client

In a Windows checkout, install stable Rust with the `x86_64-pc-windows-msvc`
toolchain and Visual Studio Build Tools with **Desktop development with C++**
and a Windows SDK. [Rust’s MSVC target guide](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html)

From the repository root in PowerShell:

```powershell
cargo check --workspace --tests --locked
cargo run --locked -p kvmctl
```

For USB administration, connect the board’s USB-UART port and confirm Windows
assigns it a COM port. See the [client README](client/README.md) for commands.

## ESP32-S3 firmware

Build from WSL with ESP-IDF 6.1.1 for `esp32s3`, the pinned
`esp-webrtc-solution` checkout, and `esp_libsrtp` 1.0.0. See the
[WSL firmware build](firmware/README.md#wsl-build) for setup and commands.
Windows PowerShell builds and board checks are documented in the
[firmware README](firmware/README.md).
