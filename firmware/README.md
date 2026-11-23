# Firmware

First product firmware increment clears the onboard addressable RGB LED at the
start of `app_main`; it has no startup animation and defaults to off.

Build the current firmware with ESP-IDF 6.1.1 and the pinned peer source
described below:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\build-transport.ps1
```

Select the LED wiring under **kvmctl hardware profile** in `idf.py menuconfig`.
GPIO48 is the default. Espressif documents GPIO48 for ESP32-S3-DevKitC-1 v1.0
and GPIO38 for v1.1, so confirm the board revision/profile before flashing.
[Espressif's FAQ](https://docs.espressif.com/projects/esp-faq/en/latest/hardware-related/development-board.html#which-gpio-is-connected-to-the-rgb-led-of-the-esp32-s3-devkitc-1-development-board)
records this revision difference.
Boards without a supported onboard RGB LED can select **No supported onboard
RGB LED**; firmware logs that LED controls are unavailable.

## M1.2 transport source build

The product adds a no-HID DataChannel echo endpoint. It accepts offer/close
commands only over the USB administration protocol; USB SDP exchange is an
internal test path, not production signaling. The firmware starts a peer only
when Wi-Fi is associated. It supports a reliable ordered `control` channel and
an unordered `motion` channel with zero retransmissions. No physical input is
captured and no HID reports are emitted.

The transport build requires ESP-IDF 6.1.1 and the pinned Espressif
`esp-webrtc-solution` checkout at commit
`3971094e2e7c2ab7cb4de59e4eae710e1902d2b4`. The script exports that source to a
content-addressed temporary component tree and applies
`verification/m0/peer-fingerprint/esp-peer-fingerprint.patch`; it verifies the
upstream revision before building. By default, it expects the checkout at
`$env:TEMP\esp-peer-v1.3.0`. If it is elsewhere, pass the checkout path:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\build-transport.ps1 -WebrtcSource C:\path\to\esp-webrtc-solution
```

The build does not flash the board. For an on-board ESP32-to-Windows test after
the firmware is installed and Wi-Fi is configured, run this from the workspace
root and replace `COMx` with the already recognized board port:

```powershell
cargo run -p kvmctl -- ports
cargo run -p kvmctl -- flash COMx .\firmware\build\kvmctl_firmware.bin .\firmware\backups\factory-app-backup.bin
cargo run -p kvmctl -- transport-test COMx
```

`flash` checks that the selected device is an ESP32-S3, reads its live ESP-IDF
partition table, and requires one non-overlapping factory app partition large
enough for the image. After confirmation it backs up that entire app partition,
verifies the backup against flash, writes and verifies only the factory app,
then resets the board. It does not write the bootloader, partition table, or
data partitions such as NVS. The backup path must not already exist, and its
directory must exist. If writing or verification fails, the CLI attempts to
restore and verify the backup. Keep that backup until the new firmware has
booted and passed its runtime check.

The command uses `IDF_PYTHON_ENV_PATH`, `IDF_TOOLS_PATH`, a standard ESP-IDF
Python installation, or Python from `PATH` to find esptool. To select another
interpreter, set `KVMCTL_ESPTOOL_PYTHON` to its full path. Esptool must be
installed in that environment.

This test creates fresh peers for initial connection and clean reconnect,
checks both channel settings, and exchanges ten echo payloads per channel. After
the first session it submits a conflicting fingerprint; after reconnect it
submits malformed SDP. Both must close the active session. The client polls USB
status during handshake and captures the Rust verifier's certificate-mismatch
warning so an identity failure is explicit even though peer state stays
`Connecting`. Raw SDP, ICE credentials, certificates, and backend logs are never
printed. HID output and physical-input capture stay disabled.

`transport-test` also reports session-local internal/PSRAM heap lows, channel
opens and received bytes, and `webrtc_peer` worker CPU time from the FreeRTOS
ESP Timer runtime clock. The CPU value covers that task only, not Wi-Fi or
other system tasks.

Fixture mode is available only through `transport-test`. These public test
keys remain on the Windows host and must never be installed on a device or used
for enrollment. To calculate the old A fingerprint in SDP format:

```powershell
$fixtures = Get-Content .\verification\m0\peer-fingerprint\fixtures\manifest.json -Raw | ConvertFrom-Json
$fingerprintA = $fixtures.fixtures.certificate_a.certificate_der_sha256 -replace '([0-9a-fA-F]{2})(?=.)', '$1:'
```

Run matching-fingerprint baselines with fixture A and fixture B presented by
Windows. In each run, the ESP32 verifies the presented fixture and Windows
verifies the ESP32's generated certificate. Both should exchange payloads,
close after the conflicting-SDP check, reconnect, then close on malformed SDP:

```powershell
$env:KVMCTL_TEST_CERT = '.\verification\m0\peer-fingerprint\fixtures\certificate_a.rtc.pem'
cargo run -p kvmctl -- transport-test COMx
$env:KVMCTL_TEST_CERT = '.\verification\m0\peer-fingerprint\fixtures\certificate_b.rtc.pem'
cargo run -p kvmctl -- transport-test COMx
Remove-Item Env:KVMCTL_TEST_CERT
```

For the ESP32 receiving-path negatives, set `KVMCTL_TEST_EXPECTED_FINGERPRINT`
to `$fingerprintA` and use certificate B, then renewed A. The board must report
an explicit fingerprint rejection before opening either channel. For the
Windows receiving-path negative, set `KVMCTL_TEST_REMOTE_FINGERPRINT` to
`$fingerprintA`; after a valid baseline, the client must report the explicit
verifier rejection with zero opened channels and zero application messages.
The board's production certificate is tested against fixture A; fixture keys
are never installed on hardware. `KVMCTL_TEST_OFFER_FINGERPRINT_FAULT` and
`KVMCTL_TEST_ANSWER_FINGERPRINT_FAULT` accept `missing`, `malformed`, or
`duplicate` to exercise fail-closed SDP handling. Clear all `KVMCTL_TEST_*`
environment variables after each run. Each negative run must be preceded by
its valid matching baseline; timeout, missing valid baseline, or a generic
connection failure is inconclusive.

## Host-side firmware guard test

`test_signaling_guards.py` runs selected production C functions on the host. It
uses Python's standard library to compile and load a temporary shared library;
the harness stubs ESP-IDF services and injects persistence failures. It checks
revocation retry and transport/session guards, not ESP-IDF compatibility or real
NVS behavior. It does not build or flash firmware.

### WSL or Linux (recommended if available)

On Ubuntu, install Python 3, Clang, and the system linker:

```sh
sudo apt update
sudo apt install python3 clang binutils
```

From the repository root in WSL/Linux, run:

```sh
python3 firmware/test_signaling_guards.py
```

For this Windows checkout, start WSL at the workspace from PowerShell:

```powershell
wsl --cd "/mnt/c/Users/$($env:USERNAME)/workspace/kvmctl"
```

Then run the two Linux commands above. Change the path if the workspace moved. See the
[Microsoft WSL setup guide](https://learn.microsoft.com/en-us/windows/wsl/setup/environment)
and [Ubuntu Clang setup](https://ubuntu.com/developers/docs/howto/clang-setup/).

### Native Windows (alternative)

Install Python 3 and either LLVM (Clang plus LLD) or the Visual Studio C++ Build
Tools. For LLVM, use the [official releases](https://github.com/llvm/llvm-project/releases)
and ensure `clang.exe` and `ld.lld.exe` are in the same `bin` directory. For
MSVC, run from a Visual Studio Developer PowerShell so `cl.exe` and `link.exe`
are available together.

```powershell
# LLVM
python .\firmware\test_signaling_guards.py --clang "C:\Program Files\LLVM\bin\clang.exe"

# Or MSVC, from Developer PowerShell
$cl = (Get-Command cl.exe).Source
python .\firmware\test_signaling_guards.py --msvc $cl
```

Python is available from the [official Windows downloads](https://www.python.org/downloads/windows/).
No Python packages are required. The temporary `.so` or `.dll` is removed when
the script exits.

## On-device revoke-persistence retry check

The optional test build injects exactly one failed per-client revoke save before
calling NVS. The retry uses the real NVS read/write/commit path. Enable it only
for this check; normal builds leave it disabled. This does not simulate a flash
hardware fault.

From the workspace root in PowerShell, build the test image:

```powershell
$env:KVMCTL_TEST_REVOKE_PERSIST_FAIL_ONCE = '1'
try {
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\firmware\build-transport.ps1
    if ($LASTEXITCODE -ne 0) { throw 'Test firmware build failed.' }
} finally {
    Remove-Item Env:KVMCTL_TEST_REVOKE_PERSIST_FAIL_ONCE -ErrorAction SilentlyContinue
}
```

Use `kvmctl flash` to write the app image only and create a fresh backup path.
It preserves NVS. Then get the target fingerprint with `clients COM6` and run:

```powershell
cargo run --offline -p kvmctl -- revoke COM6 YOUR_CLIENT_FINGERPRINT
cargo run --offline -p kvmctl -- clients COM6
cargo run --offline -p kvmctl -- revoke COM6 YOUR_CLIENT_FINGERPRINT
cargo run --offline -p kvmctl -- clients COM6
```

Replace `YOUR_CLIENT_FINGERPRINT` with the selected client's full fingerprint.
The first revoke must return an error and remove that client from the live list;
the second must succeed and leave it absent. Do not reboot between those calls:
the retry-pending marker is in RAM until the retry saves the updated list.

With the retry successful, rebuild without the environment variable and flash
that normal image app-only, using another fresh backup path. The flash resets the
board. Check `clients COM6` again; the revoked fingerprint must remain absent.
Re-enroll it afterward only if restoring that client's access is desired. Do not
erase or fill NVS to manufacture a storage failure.
