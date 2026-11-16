# kvmctl execution plan

Status: M0 in progress. Architecture and security requirements live in [design.md](design.md); this document tracks order, gates, and evidence.

## Current evidence

| Item | Status | Evidence / next check |
| --- | --- | --- |
| Private local Wi-Fi separate from hotel network | Set up, user reported | Reported 2026-11-15; topology and isolation not independently tested |
| ESP32-S3 hardware and batch | User confirmed; boards 1 and 2 identified | Three DORHEA ESP32-S3-DevKit C N16R8 boards (Amazon ASIN B0CKXJLP4B, 3-pack), each with two USB-C ports. Product docs list USB-C, 5V/GND, and 3V3/GND power options; only USB-C has been used. Boards 1 and 2 report ESP32-S3 rev 0.2, 16 MB flash, and 8 MB PSRAM. Board 3 remains sealed and untested. |
| COM/native USB topology | Boards 1 and 2: CH343 UART0 data + native HID coexistence confirmed | CH343 bridges (`1A86:55D3`, COM3/COM6) remain active while native USB enumerates keyboard and mouse HID (`303A:4004`). The repository M0 probe returned a 4-byte count for `PING` on both. Native USB CDC (`303A:4001`) and ROM USB-Serial/JTAG (`303A:1001`) were also observed in their respective modes. Internal routing details, pin-powered modes, and CDC+HID on one USB connection remain untested. |
| Board 2 RST reboot | Probe recovered; ROM boot-image SHA mismatch remains open | User pressed RST once with BOOT untouched. Probe reached ready and accepted PING, but ROM reported a SHA mismatch while loading the existing second-stage bootloader at `0x0`; see evidence below. RST/recovery gate is partial, not passed. |
| Board Wi-Fi provisioning | Local test form and RAM-only bench firmware built; no credentials entered | The Windows PowerShell form sends SSID/password only over local COM serial and displays status. Board 2 has the updated image; board 1 remains on the no-Wi-Fi probe. No Wi-Fi association result exists. |
| mDNS, VPN compatibility, HTTPS/WSS, WebRTC | Host route state observed with Surfshark connected; board behavior untested | The updated board image can start hostname mDNS after association, but has not received credentials. Hostname reachability, DNS-SD, VPN/mDNS behavior, HTTPS/WSS, and board WebRTC remain untested. |
| Stack documentation review | Host compile and Rust loopback passed; ESP32 secure stack not accepted | ESP-IDF C + `esp-webrtc-solution` `v1.3.0`; Windows Rust + `webrtc-rs` `0.21.1`. The upstream S3 peer demo builds with IDF v6.1.1 under a temporary conservative clock configuration. Two Rust peers exchange DataChannel messages on loopback. The HID-only heap baseline is measured below. The opaque ESP32-S3 peer archive's SDP-to-DTLS fingerprint enforcement is unproved; secure signaling, ESP32-to-Rust interop, and full-stack resource use remain untested. See [design](design.md). |
| Firmware/client implementation | M0 bench firmware only; product implementation not started | Firmware source is in `verification/m0/usb-hid-uart`; the local setup form is `verification/m0/wifi-setup.ps1`. The board 2 image has Wi-Fi/mDNS code, but Wi-Fi starts only on a UART command. The probe reads no physical input and sends no HID reports. Product client and firmware work remain unstarted. |

### Read-only host inventory (2026-11-15)

Procedure: queried installed commands and Rust targets; ran `usbipd list`, `pnputil /enum-devices /connected /class Ports`, and `/class USB`; inspected Windows interface state with `ipconfig /all` and `netsh interface show interface`. No firmware was flashed and no settings changed.

| Item | Observed result | Gate status |
| --- | --- | --- |
| Host Rust | `rustc 1.97.1`, `cargo 1.97.1`, `rustup 1.29.0`; active `stable-x86_64-pc-windows-msvc`; only `x86_64-pc-windows-msvc` target installed. Rust commands work but emit a profile-path canonicalization warning. | Windows client toolchain present; ESP target/toolchain absent. |
| ESP firmware tools | `espup`, `idf.py`, `esptool.py`, `espflash`, `python`, and `py` not found on `PATH`. | Not ready to build or flash firmware. |
| Serial devices | `pnputil` reported no connected `Ports` devices; `Win32_SerialPort` query returned no rows. | No board COM interface visible. |
| USB devices | `usbipd list` showed attached ASUS, Logitech, and Intel Bluetooth devices; no Espressif device. Windows PnP showed three Intel USB xHCI controllers, two USB root hubs, and a USB4 router. | Board connector-to-host-controller mapping unknown. |
| Network/VPN | Ethernet `192.168.50.2` identified by user as private KVM network; mDNS probe got no reply | Wi-Fi SSID/band and isolation not independently checked; Surfshark adapter disconnected; board Wi-Fi and reachability remain untested. |
| Repository | `git ls-files` listed only `docs/plan.md`, `docs/design.md`, and `docs/execution-plan.md`. | No implementation or capture path exists; live physical-input capture remains disabled. |

This initial host inventory does not pass USB coexistence, provisioning, discovery, VPN, TLS/WSS, WebRTC, or resource gates. Board connection and enumeration evidence follows below; remaining checks require firmware and the ESP toolchain.

### Network route snapshot (2026-11-15)

Procedure: read `ipconfig /all` and `route print -4`. PowerShell adapter/CIM queries and `netsh wlan show interfaces` returned access denied without elevation. The user identified Ethernet `192.168.50.2` as the private KVM network; subsequent mDNS probes were bound to that interface only.

| Interface | Observed result | Gate status |
| --- | --- | --- |
| Wi-Fi | `172.16.0.228/23`, default gateway `172.16.0.1` | Current IPv4 default route; not used for discovery. |
| Ethernet | Static `192.168.50.2/24`, no default gateway; user identified it as the private KVM network | Discovery probe was explicitly bound to this interface. Board reachability is untested. |
| Tailscale | Adapter has `169.254.83.107/16`, no default route | No routed VPN path appears in the IPv4 route table. |
| Surfshark OpenVPN | Adapter reports media disconnected | VPN compatibility/on-state behavior remains untested. |

Sent PTR queries to `224.0.0.251:5353` from `192.168.50.2` for `_services._dns-sd._udp.local` and `_kvmctl._tcp.local`. Both timed out after 2 seconds with no response. This does not distinguish unavailable multicast, no advertiser, or a board that is not provisioned; discovery gate remains open. No packets were sent on Wi-Fi `172.16.0.228`.

### Surfshark-connected route check (2026-11-15)

After the user connected Surfshark, read `ipconfig /all` and `route print -4`. Surfshark's WireGuard adapter had `10.14.0.2/16`; split default routes `0.0.0.0/1` and `128.0.0.0/1` pointed to that adapter. The explicit `192.168.50.0/24` route remained on Ethernet `192.168.50.2`, and Windows still listed multicast `224.0.0.0/4` routes on Ethernet and Wi-Fi. No board mDNS query was possible because the board had not joined the private Wi-Fi. This confirms route-table visibility only; VPN/mDNS compatibility remains untested.

### Board USB enumeration (2026-11-15)

Procedure: connected the board's labeled `COM` and `USB` ports to the same Windows laptop. Ran `pnputil /enum-devices /connected /class Ports`, `/class USB`, and `/class HIDClass`, plus `usbipd list`. No settings changed; no firmware was flashed; no serial data was sent.

| Path | Observed device | Result |
| --- | --- | --- |
| `COM` connector | WCH `USB-Enhanced-SERIAL CH343 (COM3)`, VID:PID `1A86:55D3` | Separate USB-UART bridge enumerated. |
| `USB` connector | Espressif composite USB device, VID:PID `303A:4001`; child `USB Serial Device (COM4)` | Native USB CDC serial interface enumerated. |
| Both connected | COM3 and COM4 both present in PnP and `usbipd` inventories | Dual-port enumeration confirmed simultaneously. |
| HID | No Espressif HID interface present in the connected HID-class inventory | At this initial read-only inventory the old firmware exposed no HID; the later M0 HID/UART probe is documented below. |

Espressif's ESP-IDF TinyUSB serial example uses VID:PID `303A:4001`; this identifies the current USB CDC function, not USB-Serial-JTAG. See [TinyUSB serial example](https://github.com/espressif/esp-idf/tree/master/examples/peripherals/usb/device/tusb_serial_device). The user-supplied Amazon listing identifies the board as DORHEA ESP32-S3-DevKit C N16R8 and the package as three boards; its product description permits power from either/both USB-C ports, 5V/GND pins, or 3V3/GND pins. M0 has used USB-C only; pin-powered modes are untested. The photo matches the VCC-GND YD-ESP32-S3 Core Board layout. VCC-GND's vendor docs describe separate USB-C paths: CH343P USB-to-UART and native USB-OTG on GPIO19/20; the TX/RX header pins are U0TXD/U0RXD GPIO43/44. See the [product listing](https://www.amazon.com/dp/B0CKXJLP4B) and [VCC-GND board documentation](https://github.com/vcc-gnd/YD-ESP32-S3). This strongly identifies the expected UART0/CH343 route, but the exact PCB revision is not printed in the photo. The initial UART prompt was not visible; the later UART0 retest below confirms application data transfer while HID is active. Other boards and USB routing remain untested. Do not mark the full M0 USB gate passed.

Private LAN is the development environment. Hostile-LAN identity and authorization requirements remain unchanged.

### ESP-IDF toolchain installation and read-only checks (2026-11-15)

Installed the official Espressif Installation Manager CLI `eim` v0.21.1 after verifying its Windows x64 download against Espressif's published SHA-256. Installed ESP-IDF v6.1.1 for target `esp32s3`. The source is under `C:\Users\night\AppData\Local\Temp\kvmctl-m0-idf-v0.21.1\idf\v6.1.1\esp-idf`; toolchain files are under `C:\Espressif\tools`.

| Check | Observed result | Gate status |
| --- | --- | --- |
| `eim list` | `v6.1.1 [ok]` at the configured ESP-IDF path | Installed; CLI version verified |
| `idf.py --version` through EIM | `ESP-IDF v6.1.1` | Host tool version confirmed |
| `esptool.py version` through EIM | `esptool v5.6.0` | Host flashing utility confirmed |
| `esptool.py --chip esp32s3 --port COM4 --before no-reset chip-id` | No response from application-mode COM4 | Expected incomplete probe; ROM download mode was needed. No erase or flash command used |
| ROM chip read on COM5 | After the user entered ROM download mode, VID:PID changed to `303A:1001`; COM3 remained present. `chip-id` reported ESP32-S3 QFN56 rev 0.2, 40 MHz crystal, USB-Serial/JTAG, and 8 MB embedded PSRAM. ESP32-S3 exposes no unique chip ID; esptool read the MAC instead, omitted here. | First board identity and reported PSRAM confirmed. The command uploaded a RAM-only stub and esptool reset via RTS after reading; no flash contents changed. |
| ROM flash read on COM5 | `esptool.py --before no-reset --after no-reset --no-stub flash-id` reported manufacturer `1c`, device `7118`, 16 MB, quad data lines, 3.3 V; command ended with “Staying in bootloader.” | First board flash capacity and ROM-mode read confirmed; no reset, erase, or write. Usable runtime memory remains unmeasured. |
| Espressif peer-demo build | `esp-webrtc-solution` tag `v1.3.0`; target `esp32s3`; `idf.py --no-ccache build` passed with IDF v6.1.1. Manifest resolved `esp_peer` 1.5.3 and `esp_libsrtp` 1.0.0. | Host compile passed only; nothing flashed and no on-device behavior verified |
| Peer-demo image and static memory | `peer_demo.bin`: `0xF65E0` bytes; app partition reports 34% free. `idf.py size`: Flash Code 750,964 bytes; Flash Data 150,168 bytes; total image 1,009,010 bytes; DIRAM 132,114 / 341,760 bytes (209,646 remain); RTC slow 36 / 8,192; RTC fast 24 / 8,192. | Static host build report only. Dynamic heap, PSRAM use, CPU, connection, and sustained traffic remain unmeasured |
| Native Rust DataChannel proof | Official `webrtc-rs` tag `v0.21.1`; `cargo check` and `cargo build --offline` passed for `x86_64-pc-windows-msvc`. Ran its offer/answer examples with ICE servers disabled, UDP bound to `127.0.0.1`, and the temporary HTTP signaling helper bound to loopback. Both peers reached `connected`; `data` opened and `offer-0/1` and `answer-0/1` exchanged. | Windows Rust loopback only. HTTP signaling was unauthenticated and local; no WSS pinning, client signatures, ESP32 interop, or LAN/VPN behavior tested |

The upstream peer demo README states that it uses SoftAP, audio, simulated SDP, and two local ESP peers; it is not an ESP-to-Rust secure-session test. Its checked-in S3 defaults use a 4 MB flash setting and 120 MHz flash/PSRAM options. For this compile-only check, the temporary checkout used 16 MB flash, 160 MHz CPU, 80 MHz flash, 80 MHz PSRAM, and HPM disabled; no repository source or board was changed. This configuration avoids the sample's flash-model warning, matches IDF 6.1.1's S3 MSPI timing table, and does not establish supported maximum board speeds.

### First-board HID coexistence test (2026-11-15)

Procedure: read the board partition table at `0x8000` and read-only security eFuses. The existing table has NVS at `0x9000`, PHY data at `0xF000`, a `factory` app at `0x10000` sized 1,984 KiB, and VFS at `0x200000` sized 6 MiB. Secure boot is disabled and `SPI_BOOT_CRYPT_CNT=0`. Backed up the original factory-app region to `C:\Users\night\AppData\Local\Temp\kvmctl-m0-original-factory-app.bin` before writing.

Built Espressif's [ESP-IDF v6.1.1 TinyUSB HID example](https://github.com/espressif/esp-idf/tree/v6.1.1/examples/peripherals/usb/device/tusb_hid) in a temporary directory, using `esp_tinyusb` 2.4.0 and TinyUSB 0.21.0~6. Removed the example's button read and every keyboard/mouse report send; static search confirmed no GPIO input calls or HID report-send calls remain. The first image is 193,552 bytes and fits the existing factory slot. A reproducible no-report UART/HID probe project is now in `verification/m0/usb-hid-uart`.

Flashed only the app image at `0x10000`; left the bootloader, partition table, NVS, and VFS unchanged. esptool verified the written image hash. The board initially stayed in ROM mode after the automatic RTS reset; after a manual RESET with BOOT released, it booted the test app.

| Observation | Evidence | Result |
| --- | --- | --- |
| Native USB HID | `usbipd list` showed `303A:4004`; `pnputil` showed `HID\VID_303A&PID_4004&Col01` as `HID Keyboard Device` and `Col02` as `HID-compliant mouse`. | Keyboard and mouse interfaces enumerate on Windows without sending reports. |
| Serial alongside HID | `usbipd list` and `pnputil /class Ports` showed WCH CH343 `COM3` (`1A86:55D3`) at the same time. Native USB no longer appeared as CDC or ROM JTAG in this app mode. | First-board COM-bridge + native-HID coexistence confirmed. UART0 data transfer during HID operation was later verified; board 2 is documented below. |
| Board-level gate | No physical input path exists in the temporary firmware; no HID reports were sent. USB-OTG/controller routing and same-port CDC+HID were not tested. | The coexistence subtest passes on boards 1 and 2; the overall USB topology gate remains partial. |

The original app backup remains in the local temp directory above; it is not in the repository. No live physical-input capture was enabled.

After flashing the earlier Wi-Fi probe and the user's RESET, the first COM3 prompt was not visible. A 40-character dummy line was sent; it was designed to be rejected before Wi-Fi setup. No SSID or password was sent and no network connection was attempted. PnP still listed the native keyboard and mouse interfaces. A later no-Wi-Fi UART0 app verified COM3 data transfer; see the next section.

### UART0 data with native HID active (2026-11-15)

Created and built a repository ESP-IDF TinyUSB HID probe with GPIO input/report-send code absent and Wi-Fi, mDNS, and credential-input code disabled. Keyboard and mouse descriptors remain active. UART0 prints a ready marker and reports only the length of received lines. It does not read physical inputs or send HID reports; no credentials were entered.

The reproducible project is in [verification/m0/usb-hid-uart](../verification/m0/usb-hid-uart). ESP-IDF v6.1.1 built it from the repository. The image is 204,560 bytes (0x31F10), with 80% of the temporary 1 MiB app slot free. SHA-256: e3fbb2d4e28570883b0b3de366c6aec2a243543737accd2e44f7327dc99161bf.

Flashed the repository-built application at 0x10000 over COM3. esptool v5.6.0 connected to ESP32-S3 QFN56 rev 0.2, verified the written data hash, and hard-reset the board. Bootloader, partition table, NVS, and VFS were untouched. Opened COM3 at 115200 8-N-1 with DTR/RTS deasserted and sent the literal PING line. COM3 returned M0 UART0 received 4 bytes and the HID-only heap baseline: internal free 383,651 bytes, minimum internal free since boot 351,620 bytes, PSRAM free 8,386,128 of 8,388,608 bytes. This reading is with TinyUSB HID active and no Wi-Fi or WebRTC stack. The startup marker was not observed because the serial port was opened after the board had booted.

pnputil /enum-devices /connected showed native USB VID_303A&PID_4004 with a started HID Keyboard Device child and started HID-compliant mouse child, while WCH VID_1A86&PID_55D3 remained started as CH343 COM3. First-board UART0 data transfer and native keyboard/mouse enumeration therefore coexist. Board 2 confirms the same subgate, as recorded below. These tests do not prove native USB-OTG routing details, same-port CDC+HID, or pin-powered behavior. No input was captured and no HID reports were sent.

### Second-board HID/UART coexistence test (2026-11-15)

The user unplugged board 1 and connected board 2 to both USB-C ports on the same PC; BOOT and RESET were untouched. Initial PnP inventory showed native USB CDC on COM4 (`303A:4001`) and WCH CH343 on COM6 (`1A86:55D3`) simultaneously. Read-only `chip-id` identified ESP32-S3 QFN56 rev 0.2, 40 MHz crystal, and 8 MB embedded PSRAM; `flash-id` reported 16 MB quad flash at 3.3 V. Read-only eFuse summary showed `SECURE_BOOT_EN=0`, `SPI_BOOT_CRYPT_CNT=0`, download mode enabled, and USB OTG/USB Serial-JTAG disable fuses unset.

The partition table has NVS at `0x9000`/`0x6000`, PHY data at `0xF000`/`0x1000`, factory app at `0x10000`/`0x1F0000` (1,984 KiB), and VFS at `0x200000`/`0x600000` (6 MiB). A 460800-baud read of the factory app stopped at about 42% with `Packet content transfer stopped`; retrying at 115200 completed. The complete 2,031,616-byte app backup is `C:\Users\night\AppData\Local\Temp\kvmctl-m0-board2-original-factory-app.bin`, SHA-256 `fe09a58b696136c530fcab1993b1522b63ac73979ba7fcfa25872bb491164cc9`.

Flashed the repository-built no-report M0 app only at `0x10000` over COM6 at 115200 baud. Image size is 204,560 bytes; SHA-256 `e3fbb2d4e28570883b0b3de366c6aec2a243543737accd2e44f7327dc99161bf`. esptool v5.6.0 reported `Hash of data verified` and hard-reset the board. Bootloader, partition table, NVS, PHY data, and VFS were not written.

After boot, opened COM6 at 115200 8-N-1 with DTR/RTS deasserted and sent `PING`. UART0 returned `M0 UART0 received 4 bytes` and heap counters: internal free 383,651 bytes, minimum internal free 351,620 bytes, PSRAM free 8,386,128 of 8,388,608 bytes. PnP reported native USB `303A:4004` with started keyboard (`Col01`) and mouse (`Col02`) children, while CH343 COM6 stayed started. Thus board 2 confirms the same dual-port serial/HID operation and HID-only heap baseline observed on board 1. Firmware does not read physical input and sent no HID reports.

This passes the two tested boards' CH343 UART0 + native-HID coexistence subgate. Board 3 remains sealed and untested. USB-OTG routing details, same-port CDC+HID, pin-powered modes, Wi-Fi, and full-stack resource use remain open; do not infer the full M0 USB or stack gate passed.

### Board 2 RST-only reboot (2026-11-15)

The user tapped RST once on board 2 without pressing BOOT. A passive COM6 capture showed ROM reset reason `rst:0x1 (POWERON)`, then `SHA-256 comparison failed`: calculated `cbd95a816bf0b42be96621cc70ea54ca844ad51f007c6baddbfea1444d8f0913`, expected `48304bcd6dd6bd250bc1ad011a0549e9b13e819f8aa98e34f1df27c016a39ae9`, followed by `Attempting to boot anyway...`. The M0 probe then reached `M0 UART0 probe ready`; a subsequent COM6 `PING` returned `M0 UART0 received 4 bytes` and the expected HID-only heap counters. No physical input was read and no HID report was sent.

The warning precedes the second-stage bootloader's `entry` and is for the image at flash offset `0x0`, not the M0 application at `0x10000`. Read-only `esptool v5.6.0 read-flash 0x0 0x8000` saved the current boot region to `%TEMP%\kvmctl-m0-board2-bootregion.bin`; no bootloader bytes were written during M0. Parsing the 18,704-byte bootloader image from that readback found a valid image checksum (`0xc6`) but an invalid validation SHA: the ROM's calculated digest matches the bootloader bytes through offset `0x48f0`, while the stored expected digest at `0x48f0` differs. The repository M0 app image itself reports a valid validation hash with `esptool image-info`. Espressif's [ESP-IDF v6.1.1 bootloader guide](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-guides/bootloader.html) places the second-stage bootloader at offset `0x0`; its [bootloader image-format guide](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32s3/api-reference/system/bootloader_image_format.html) documents the image checksum and validation hash. An [Espressif maintainer response](https://github.com/espressif/esp-idf/issues/12349#issuecomment-1758131132) confirms this SHA comparison message comes from the ROM first-stage bootloader. The reason the existing bootloader's stored SHA differs is unknown; no pre-flash bootloader backup exists, so we cannot say whether the mismatch predated M0. The board recovers into the probe, but the reset/recovery gate remains partial pending an agreed safe bootloader diagnosis; no bootloader replacement was attempted.

### Repository Wi-Fi/mDNS bench image on board 2 (2026-11-15)

Built the repository project with ESP-IDF v6.1.1, `espressif/esp_tinyusb` 2.4.0, and `espressif/mdns` 1.14.0. The app image is 852,592 bytes (`0xD0270`), SHA-256 `e3db1406bd504d64c8e4c3e0b33832505c6a2a4613c7e7c34c910cf6fe9a6c8a`; `esptool image-info` reports a valid image checksum and validation hash. It fits board 2's existing 1,984 KiB factory app partition. Firmware still enumerates HID keyboard/mouse but reads no physical input and sends no HID reports. Wi-Fi starts only after `WIFI <ssid-hex> <password-hex>` arrives on CH343 UART0; credentials use RAM storage, NVS Wi-Fi storage is disabled, and the setup form clears its fields after sending. No credentials were entered.

Before updating board 2, a read-only snapshot of `0x10000`–`0x41FFF` (204,800 bytes) was saved as `%TEMP%\kvmctl-m0-board2-app-before-wifi.bin`, SHA-256 `e1ed967ab4091c409f43c8653a5f93068fd20e2a3e8a60106f9acc458ea842d1`. `esptool image-info` found a valid footer hash for the prior app. This snapshot covers the known prior M0 app image plus 240 bytes; it is not a full app-partition backup or a pre-M0 factory backup. A separate initial attempt to read nearly the whole app partition stopped with a short/corrupt transfer and wrote nothing.

Flashed only the new app at `0x10000` over COM6 with esptool v5.6.0. It erased within the factory app partition, wrote 852,592 bytes, reported `Hash of data verified`, and hard-reset the board. Bootloader, partition table, NVS, PHY data, and VFS were not written. After boot, COM6 `PING` returned `M0 UART0 received 4 bytes`; `STATUS` returned `wifi=disabled ip=0.0.0.0 mdns=off reason=0`. The post-PING idle reading with Wi-Fi code linked but not initialized was internal free 323,591 bytes, minimum internal free 291,560 bytes, and PSRAM free 8,386,128 of 8,388,608 bytes. This is a bench idle measurement, not a full-stack or Wi-Fi-started measurement; internal headroom is lower than the earlier HID-only baseline.

The local Windows PowerShell 5.1 form is `verification/m0/wifi-setup.ps1`; it selects a COM port, accepts SSID/password locally, sends them over USB serial, and polls `STATUS`. Script parsing succeeded. Windows reports the form window as visible, but its layout was not visually inspected. No local credentials were entered and no Wi-Fi attempt occurred. A `mdns=on` status would indicate responder initialization only; network reachability still needs a host-side query. The firmware registers no DNS-SD service and provides no TLS/WSS or control listener.

### Earlier RAM-only Wi-Fi/mDNS probe attempt (2026-11-15; superseded)

An earlier build of the temporary project used ESP-IDF v6.1.1, esp_tinyusb 2.4.0, TinyUSB 0.21.0~6, and Espressif mdns 1.14.0. It had a RAM-only Wi-Fi credential prompt; the prompt was not visible and the user entered no credentials. It did not initialize NVS. At that time both tested boards ran the no-Wi-Fi UART0/HID probe; board 2 was later updated to the repository bench image described above. Board 1 remains on the earlier no-Wi-Fi image.

The earlier temporary source/build folder is under C:\Users\night\AppData\Local\Temp\kvmctl-m0-hid-enumeration; its app image was superseded and is not currently flashed. The original factory app backup remains at C:\Users\night\AppData\Local\Temp\kvmctl-m0-original-factory-app.bin. The current no-Wi-Fi probe source is in [verification/m0/usb-hid-uart](../verification/m0/usb-hid-uart); generated build output and component cache are ignored.

The earlier image enabled octal 8 MB PSRAM at 80 MHz and contained heap counters for HID-ready, Wi-Fi-started, and mDNS-ready phases. It was 854,000 bytes and fit the temporary 1 MiB app slot with 19% free. These are static build details only; no device heap/PSRAM readings were observed from that image. It was superseded by the 204,560-byte repository UART0 image above.

COM3 automatically entered ROM mode for a read-only chip check. The first flash attempt using `--before no-reset` failed to connect and wrote nothing. Retrying with automatic reset flashed only the app image at `0x10000`; esptool verified the written hash. Bootloader, partition table, NVS, and VFS were not written. The user confirmed a manual reset afterward, but no serial prompt/runtime result was observed. No Wi-Fi credentials have been entered. The earlier Wi-Fi probe did not associate or produce mDNS evidence; that bench image had no TLS/WSS or WebRTC path.

The earlier probe was intended to advertise _kvmctl._tcp with TXT markers probe=m0 and secure=off; it did not listen on the advertised port or provide TLS/WSS. Wi-Fi association, mDNS reachability, and runtime resource counters remain untested for that image. It sent no HID reports and read no physical input.

Evidence now includes the host toolchain, ESP32-S3 peer-demo compile, Rust-to-Rust loopback DataChannel, two-board chip/flash/PSRAM identification, native HID keyboard/mouse enumeration concurrent with successful CH343 UART0 data round trips, and HID-only internal-heap/PSRAM measurements on both boards. The user deferred Wi-Fi credential entry until a minimal control-app interaction UX exists. Board 3 remains sealed and untested. The full board stack gate remains open: USB routing details, Wi-Fi provisioning, mDNS reachability, VPN/mDNS behavior, secure WSS/authentication, ESP32-to-Rust WebRTC, full-stack heap/PSRAM use, CPU/network load, and sustained traffic remain unmeasured.

### Protocol/security contract review (2026-11-15)

A read-only contract audit against primary sources found implementation blockers; [design.md](design.md) now resolves the specification gaps without claiming runtime verification. USB `status` exposes endpoint identity SPKI as well as its fingerprint and TLS pin. TLS key changes require USB pin refresh/re-enrollment; protocol 1.0 has no network pin-rotation path. The signed challenge now carries capabilities before SDP, avoiding a transcript/`ready` ordering cycle. WSS CBOR framing is per reassembled binary message (16 KiB cap), and DataChannel control cap is 512 bytes (274-byte maximum keyboard-state envelope). `lease_release` and owner-scoped cleanup rules preserve another client's lease when a non-owner session fails. UART administration is defined by a complete request received on CH343 UART0, not COM-open detection.

Primary-source checks: [RFC 6455 §5.4](https://www.rfc-editor.org/rfc/rfc6455#section-5.4) permits fragmented WebSocket messages; [RFC 8122](https://www.rfc-editor.org/rfc/rfc8122) specifies SDP certificate fingerprints; [RFC 8827](https://www.rfc-editor.org/rfc/rfc8827) describes out-of-band WebRTC peer verification. The pinned [Espressif `esp_peer` v1.3.0 API](https://github.com/espressif/esp-webrtc-solution/blob/v1.3.0/components/esp_peer/include/esp_peer.h) exposes channel reliability/order settings but no public remote-fingerprint verification hook. In the IDF 6 build path, [`dtls_srtp_v6.c`](https://github.com/espressif/esp-webrtc-solution/blob/v1.3.0/components/esp_peer/src/dtls_srtp_v6.c) sets `MBEDTLS_SSL_VERIFY_OPTIONAL`; shared [`dtls_common.h`](https://github.com/espressif/esp-webrtc-solution/blob/v1.3.0/components/esp_peer/src/dtls_common.h) checks mbedTLS verification flags after the client handshake but returns success without rejecting nonzero flags. The private `remote_fingerprint` field is not read or compared by the visible C wrapper. However, the ESP32-S3 peer implementation is linked from a prebuilt `libpeer_default.a`; its SDP parser/fingerprint strings do not establish whether the archive compares the actual DTLS certificate to the SDP fingerprint. Treat this stack as unaccepted for secured control until source-level proof or a wrong-certificate test confirms fail-closed behavior before control DataChannel data or HID enablement; if it cannot be proved, patch/fork the component or choose a backend with an explicit verification path. Secure WSS, revocation persistence behavior, shared Rust/C protocol vectors, and ESP32-to-Rust interoperability still require implementation tests; no secure session gate passed.

## M0 preparation

- Separate durable requirements from implementation sequence.
- Resolve user policy questions in [plan.md](plan.md), or keep dependent work blocked.
- M0 protocol and security contracts are recorded in [design.md](design.md); network-stack measurements and cross-stack interop remain pending.
- Host inventory is captured above. No gate is passed without direct evidence from the board and private LAN.

## Milestones

### M0: Paperwork, then hardware and stack verification

- Resolve user policy questions in [the planning index](plan.md).
- Confirm repository layout and Rust workspace boundaries on paper; create the skeleton when implementation starts.
- Use the three DORHEA ESP32-S3-DevKit C N16R8 boards; verify each board's USB behavior, detected flash/PSRAM, internal heap, ESP-IDF version, and firmware/client language boundary.
- Document protocol, identity, capability, and error-state conventions.
- Define USB enrollment, per-client keys, certificate pinning, authenticated signaling, revoke/reset behavior, and one-controller lease semantics before LAN control work.
- Freeze the minimum wire protocol and test telemetry needed for scripted event comparison and latency measurement.
- Define the same-host test rig, including the ESP32 HID observer and acceptance criteria for keyboard, mouse, reconnect, and held-input cleanup. Specify clock mapping and measurement uncertainty.
- Choose WebRTC and mDNS/DNS-SD libraries after validating Windows and ESP32 constraints.
- Run the hardware and stack verification gate below. Record results in this execution plan before committing to the full vertical slice.

#### M0 hardware and stack verification gate

- Connect the board's `COM` and `USB` ports to the same Windows laptop. Confirm both enumerate, identify `COM` as USB-UART or USB-Serial/JTAG, and verify whether serial provisioning/logging remains usable while native USB runs HID.
- Confirm whether the two physical ports expose independent usable controllers or share a controller/PHY limitation. Verify native USB HID can remain attached to a target while `COM` handles enrollment and diagnostics.
- Provision Wi-Fi through the local bench setup form on `COM`; do not send credentials through chat. Test a known 2.4 GHz private network; captive-portal hotel Wi-Fi is out of scope for board onboarding.
- Use a private travel-router LAN for repeatable development. Test laptop Ethernet plus ESP32 Wi-Fi, mDNS, HTTPS/WSS, and WebRTC with Surfshark enabled; repeat with local-LAN visibility settings changed only if needed.
- Build the smallest selected WebRTC proof on the exact board and firmware stack. Measure build size, internal heap, TLS/WSS handshake, DataChannel setup, CPU, reconnect, and sustained HID-path behavior.
- Record pass/fail results and update architecture choices before M1 implementation. If WebRTC cannot fit or USB enrollment cannot coexist with HID, stop and revise the design.

M0 exit: board and USB topology identified; toolchain/library versions recorded; enrollment/session contracts documented; smallest secure WebRTC proof measured on the actual board. A configured LAN alone does not pass this gate.

### M1: ESP32-S3 vertical slice

- Build the minimal Windows client for USB enrollment, discovery, authenticated signaling, scripted input, and HID observation. M1 must not depend on the polished M2 CLI.
- Advertise the prototype through mDNS/DNS-SD with endpoint type and capabilities.
- Implement local authorization and board-hosted HTTPS/WSS signaling.
- Persist device/client identities, enrolled-client records, and revocations; verify restart behavior.
- Establish authenticated WebRTC DataChannels.
- Forward keyboard and mouse events to USB HID.
- Add scripted transport/HID test modes, ESP32 telemetry, local HID observation, and latency reports.
- Enforce one active control lease and explicit takeover.
- Add disconnect, held-input cleanup, and reconnect tests.
- Verify unauthorized clients, wrong endpoint pins, stale/replayed session data, and revoked clients cannot control HID; transport-test mode must never emit HID.

M1 exit: one enrolled Windows client drives one ESP32 through the real secure path; a second enrolled client demonstrates exclusive takeover; transport and HID bench results are recorded. Authorization and held-input checks pass for implemented scripted/HID/session paths. Live capture, hotkey switching, and emergency escape close in M2 before live input is enabled.

### M2: Native Windows client

- Build `kvmctl` CLI discovery, authorize, connect, list, and switch commands.
- Expose the M1 identity storage and USB-only administration/revoke behavior through complete CLI workflows.
- Add Windows global hotkey and target switching.
- Add `kvmctld` only when a background process is needed for persistent behavior.

M2 exit: arming, emergency escape, switching, revoke, and reconnect match the resolved design; no stuck input or unexpected routing to inactive targets.

### M3: JetKVM adapter, later

- Capture JetKVM local protocol and service-advertisement behavior.
- Implement discovery and authorization integration.
- Map JetKVM input and available video into the shared session model. JetKVM video belongs to this milestone; ESP32 remains input-only.
- Test JetKVM restart, network changes, and sleep/resume paths.

### M4: Video refinement and opt-in power features

- Refine video negotiation/display when endpoint and user settings allow it; basic JetKVM video is delivered in M3.
- Measure latency, bandwidth, CPU, and memory before optimizing.
- Add opt-in local sleep prevention.
- Add opt-in remote sleep prevention only after endpoint support and authorization are proven.

### M5: Hardening and portability

- Threat-model review, fuzz message parsing, and test downgrade/replay/identity failures.
- Package Windows client and firmware update/recovery paths.
- Evaluate Linux/macOS support without weakening Windows-first behavior.


## Acceptance checklist

Freeze report schema and exact test scripts in M0. Fixed latency targets follow the initial measured baseline. Apply session/scripted/HID checks in M1; complete live capture, target-switching, and emergency-escape checks in M2 before enabling live input.

- **Enrollment/identity:** two clients enroll independently over direct USB; LAN discovery cannot enroll a client; an unexpected endpoint identity is rejected.
- **Authorization:** unregistered/revoked clients cannot obtain control. Enrolled LAN clients cannot mutate the authorization list; enrollment, per-client revoke, and revoke-all require direct USB. USB revocation of active control releases held input and invalidates its lease. Restart never silently restores revoked access.
- **Transport mode:** deterministic events use the same authentication, WSS signaling, WebRTC, CBOR, and ESP32 decoder as live mode; no USB HID is emitted. Compare sequence numbers, normalized reports, drop/reorder counts, and queue telemetry.
- **HID bench mode:** observer filters by ESP32 Raw Input device identity; expected key/modifier/button/wheel/relative-motion reports arrive without suppressing physical devices.
- **Ownership:** exactly one active control lease; old lease invalid before new owner emits input. Test explicit rejection and no-response takeover after 10 seconds. Resolve concurrent-request policy in M0 before testing races.
- **Held-input safety:** switch, disconnect, timeout, revoke, endpoint restart, and emergency escape clear or reconcile held keys/buttons. Define heartbeat/lease expiry and maximum cleanup delay before implementation.
- **Selected-target sessions:** switching releases the old lease and closes its network session before connecting the new target. Inactive targets retain no warm WebRTC/signaling sessions. USB HID remains attached. Measure new-session setup and switch latency on the LAN; do not assume it meets an unmeasured target.
- **Reconnect:** test endpoint restart, address change, network loss, and Windows sleep/resume. Revalidate identity, authorization, capabilities, and ownership before input resumes. Recover automatically to the same selected target only while the outage has not exceeded 60 seconds; discard outage input. Test recovery just before the deadline and failure to recover beyond it, including sleep lasting over one minute. Partial handshakes must not reset the deadline. After timeout, pending connections close, retries stop, input stays disarmed, and late completions cannot resume it. Explicit target selection/connection through the app rearms using saved enrollment. Manual disconnect/disarm/escape cancels recovery immediately. Never silently select another target.
- **Latency/resources:** record transport RTT, board-local processing/queue time, same-host send-to-HID receipt, CPU, flash, PSRAM, and internal-heap use during TLS/WSS setup and sustained traffic. Report one-way timing only with clock mapping and uncertainty.
- **Recovery:** verify deliberate BOOT factory reset, RST reboot-only behavior, COM recovery, and key-loss USB re-enrollment. Document agreed erase scope and recovery authorization.

M0 sets repeat counts, cleanup deadlines, resource headroom, and pass/fail limits where these depend on board/stack behavior. M1 cannot close with these fields unresolved.

## Evidence record for each execution gate

Record results here as work proceeds:

- Date, board/module, firmware/client revision, tool/library versions.
- USB wiring, target/client roles, OS, and network/VPN configuration.
- Commands or procedure, expected behavior, actual result, and pass/fail.
- Build size, minimum free internal heap, PSRAM use, CPU, connection/reconnect behavior, and timing method/uncertainty.
- Redacted log/report paths; never credentials or private keys.
- Failures, resulting design changes, and next gate allowed to proceed.

Board identity, partition/security state, and USB coexistence evidence for boards 1 and 2 are recorded above. HID-only dynamic heap baselines exist for both, plus a board 2 idle reading with Wi-Fi code linked but not initialized. Full-stack resource measurements and all on-device Wi-Fi/mDNS/WebRTC behavior remain untested.
