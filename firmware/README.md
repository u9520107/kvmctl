# Firmware

First product firmware increment clears the onboard addressable RGB LED at the
start of `app_main`; it has no startup animation and defaults to off.

## Windows build (primary)

Build from the repository root in Windows PowerShell. Install ESP-IDF 6.1.1
for `esp32s3` with Espressif Installation Manager, including CMake, Ninja, and
the ESP-IDF Python environment. The repository build script loads the installed
profile from `C:\Espressif\tools\Microsoft.v6.1.1.PowerShell_profile.ps1`.

The repository vendors JetKVM Mini's source-built `third_party/esp_peer`
snapshot. A cached `esp_libsrtp` component may still be supplied through
`KVMCTL_LIBSRTP_SOURCE`; no prebuilt peer library is used.

```powershell
$env:PYTHONUTF8 = '1'
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\firmware\build-transport.ps1
certutil -hashfile .\firmware\build\windows-production\kvmctl_firmware.bin SHA256
```

The script verifies ESP-IDF `v6.1.1`, uses the checked-in Mini peer source and
builds `firmware\build\windows-production\kvmctl_firmware.bin`. The first build may fetch
pinned components from the ESP Component Registry. The build does not flash a
board.

## Scripted USB HID bench

Use the same Windows setup with an explicit bench build. Copy the normal image
first if it must be retained; the bench build uses its own directory under
`firmware\build`.

```powershell
Copy-Item .\firmware\build\windows-production\kvmctl_firmware.bin .\firmware\build\windows-production\kvmctl_firmware-normal.bin
$env:PYTHONUTF8 = '1'
$env:KVMCTL_HID_BENCH = '1'
try {
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\firmware\build-transport.ps1 -BuildDirectory .\firmware\build\windows-hid-bench
} finally {
    Remove-Item Env:KVMCTL_HID_BENCH -ErrorAction SilentlyContinue
}
certutil -hashfile .\firmware\build\windows-hid-bench\kvmctl_firmware.bin SHA256
```

For a build without registry access, set `KVMCTL_HID_BENCH_COMPONENTS` to a directory containing
the pinned `espressif__esp_tinyusb` and `espressif__tinyusb` components. Copy the
bench app to the Windows build directory and use
`kvmctl flash` with a fresh backup path. The bench image enumerates a USB
keyboard and mouse. It emits script reports only for the current authenticated
lease after an explicit arm request. See the [Windows client bench command](../client/README.md#scripted-usb-hid-bench).

The HID configuration advertises USB remote wake and the firmware requests
remote wake when the first queued input arrives during USB suspend. The board
must remain powered while the target sleeps, and the target firmware/BIOS must
allow USB wake; hibernate, shutdown, or a target that removes USB power cannot
be awakened by this path.

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
when Wi-Fi is associated. It supports Mini's reliable ordered `rpc` and
`hidrpc` channels plus ordered and unordered `hidrpc-unreliable-*` channels
with zero retransmissions. HIDRPC uses Mini's binary reports; `rpc` uses
JSON-RPC text. No physical input is captured and no HID reports are emitted.

The transport build requires ESP-IDF 6.1.1 and uses the vendored Mini peer
source directly. Its source-built SCTP advertises twenty negotiated stream
IDs (ten slots per direction), and the firmware logs DCEP labels, stream IDs,
association close state, application bytes, and channel-open counts. DTLS
fingerprint rejection remains fail-closed.

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\build-transport.ps1 -WebrtcSource C:\path\to\esp-webrtc-solution
```

An older Espressif checkout can still be passed for the managed libSRTP cache,
but it is not used as the peer implementation when the vendored Mini source is
present.

The build does not flash the board. For an on-board ESP32-to-Windows test after
the firmware is installed and Wi-Fi is configured, run this from the workspace
root and replace `COMx` with the already recognized board port:

```powershell
cargo run -p kvmctl -- ports
cargo run -p kvmctl -- flash COMx .\firmware\build\windows-production\kvmctl_firmware.bin .\firmware\backups\factory-app-backup.bin
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

## Recovery and rollback

Use the app-only `flash` flow for firmware recovery. It reads and validates the
live partition table, backs up and verifies the complete factory app partition,
writes only the factory app, verifies the new image, and attempts an automatic
restore from the verified backup if write or verification fails. Record the
image path, byte count, SHA-256, backup path, byte count, SHA-256, board
capabilities, COM port, and post-reset `status` output. Keep the verified
backup until bring-up passes; rollback is the same `flash` command with the
verified backup as the image and a new backup destination.

Do not erase the whole flash during normal bring-up. Do not write the
bootloader or partition table without separate explicit approval and a
manufacturer-matched image, backup, and rollback plan. Board 2's existing
bootloader validation-SHA mismatch is a separate device finding; readback is
not a pre-flash backup and is not permission to replace it.

The running firmware watches BOOT on GPIO0. A continuous 10-second hold erases
all kvmctl-managed NVS (`kvmctl_wifi`, `kvmctl_id`, `kvmctl_auth`, and
`kvmctl_cfg`) and reboots. A shorter hold preserves state. RST only reboots.
Verify reset through USB `status`, `clients`, and deliberate `enroll` output.
For a lost Windows client key, use direct USB with explicit confirmation:

```powershell
cargo run --offline -p kvmctl -- ports
cargo run --offline -p kvmctl -- status COMx
cargo run --offline -p kvmctl -- recover COMx
cargo run --offline -p kvmctl -- clients COMx
```

`recover` replaces this user's DPAPI-protected client key, enrolls it through
the selected COM port, and prints the identity, TLS pin, and new client
fingerprint. It does not erase endpoint state or revoke other clients.

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
opens and received bytes, peer-task CPU time, total SoC FreeRTOS runtime,
maximum board event-processing time, and maximum response-queue residence.
These CPU and timing values use the ESP Timer runtime clock and are local to
the measured transport session.

## Host-side firmware regression

Run the complete host-only regression from an x64 Visual Studio Developer
PowerShell at repository root:

```powershell
.\scripts\test-host.ps1
```

This runs all seven firmware checks, writes
`firmware\build\host-regression\live-input-fixture.bin`, sets
`KVMCTL_LIVE_FIXTURE`, explicitly runs the ignored
`firmware_rule_rearm_and_capture_handoff` test, and runs the locked Rust
workspace suite. Use `-Online` only when locked dependencies are not available
offline. The runner fails early with the missing compiler prerequisite instead
of silently omitting C-backed checks. Hardware checks remain opt-in below.

`test_signaling_guards.py` runs selected production C functions on the host. It
uses Python's standard library to compile and load a temporary shared library;
the harness stubs ESP-IDF services and injects persistence failures. It checks
two-peer transport isolation, signaling deadlines, and revocation retry.
ESP-IDF compatibility and real NVS behavior require on-device checks.

### Native Windows

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
