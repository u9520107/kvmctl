# kvmctl execution plan

Status: M0 bootstrap closed; M1 implementation is next. Plan revised 2026-11-16 to build the product and verify/harden each feature together. Architecture and security requirements live in [design.md](design.md).

The milestone section is the current plan. Dated bench records preserve what was observed under the earlier plan; references there to open M0 gates do not block M1. Board numbers identify test samples only. No third-board qualification is required, and no recorded COM port, IP, MAC, or serial number is a product constant. Outstanding test results remain outstanding after this planning change.

## Current evidence

| Item | Status | Evidence / next check |
| --- | --- | --- |
| Private local Wi-Fi separate from hotel network | Set up, user reported | Reported 2026-11-15; topology and isolation not independently tested |
| ESP32-S3 hardware configuration | Representative samples identified; bootstrap sufficient | N16R8 samples report ESP32-S3 rev 0.2, 16 MB flash, and 8 MB PSRAM, with CH343 UART and native USB. Product targets compatible hardware capabilities, not these specific units; no batch-wide qualification gate. |
| COM/native USB topology | Boards 1 and 2: CH343 UART0 data + native HID coexistence confirmed | CH343 bridges (`1A86:55D3`, COM3/COM6) remain active while native USB enumerates keyboard and mouse HID (`303A:4004`). The repository M0 probe returned a 4-byte count for `PING` on both. Native USB CDC (`303A:4001`) and ROM USB-Serial/JTAG (`303A:1001`) were also observed in their respective modes. Internal routing details, pin-powered modes, and CDC+HID on one USB connection remain untested. |
| Sample bootloader/RST finding | Device issue retained; product recovery work in M1.5 | Probe recovered after RST, but this sample's existing bootloader reports a SHA mismatch. Preserve backups/evidence and use a safe recovery procedure; this does not block unrelated product implementation. |
| Board Wi-Fi provisioning | Board 2 associated and received DHCP address | Local credential entry connected Board 2 at `192.168.50.137`; ICMP succeeds from Ethernet with Surfshark active. Board 1 remains on the no-Wi-Fi probe. Credentials remain RAM-only. See dated evidence below. |
| mDNS, VPN compatibility, HTTPS/WSS, WebRTC | Unicast mDNS works; multicast unresolved; unmodified backend fails fingerprint gate | The restored Wi-Fi probe publishes no DNS-SD service and implements no HTTPS/WSS or WebRTC. Separate two-peer board firmware passed a DataChannel baseline but accepted mismatched fingerprints in both DTLS roles. LAN reachability with Surfshark active does not pass discovery or secure-stack gates. |
| Stack documentation review | Board transport baseline passed; unmodified ESP32 secure stack rejected | ESP-IDF C + `esp-webrtc-solution` tag `v1.3.0` / component `esp_peer` 1.5.3; Windows Rust + `webrtc-rs` 0.21.1. Exact board runtime test allowed payloads with wrong SDP fingerprints in both roles. A source patch is being evaluated separately. Secure signaling, ESP32-to-Rust interop, and full-stack resource use remain untested. See [design](design.md). |
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

This passes the tested samples' CH343 UART0 + native-HID coexistence subgate. Other USB configurations and full-stack operation were not established by this test. Under the revised plan, representative evidence is sufficient for bootstrap; further checks belong to their implemented features rather than batch qualification.

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

At this point in the original bench work, evidence included the host toolchain, ESP32-S3 peer-demo compile, Rust-to-Rust loopback DataChannel, sample chip/flash/PSRAM identification, native HID enumeration alongside CH343 UART0 data, and HID-only heap measurements. The user deferred credential entry until a minimal control-app interaction existed. Network, secure transport, and full-stack measurements were still outstanding; later evidence follows. The revised implementation slices own unfinished work without requiring additional batch samples.

### Protocol/security contract review (2026-11-15)

A read-only contract audit against primary sources found implementation blockers; [design.md](design.md) now resolves the specification gaps without claiming runtime verification. USB `status` exposes endpoint identity SPKI as well as its fingerprint and TLS pin. TLS key changes require USB pin refresh/re-enrollment; protocol 1.0 has no network pin-rotation path. The signed challenge now carries capabilities before SDP, avoiding a transcript/`ready` ordering cycle. WSS CBOR framing is per reassembled binary message (16 KiB cap), and DataChannel control cap is 512 bytes (274-byte maximum keyboard-state envelope). `lease_release` and owner-scoped cleanup rules preserve another client's lease when a non-owner session fails. UART administration is defined by a complete request received on CH343 UART0, not COM-open detection.

Primary-source checks: [RFC 6455 §5.4](https://www.rfc-editor.org/rfc/rfc6455#section-5.4) permits fragmented WebSocket messages; [RFC 8122](https://www.rfc-editor.org/rfc/rfc8122) specifies SDP certificate fingerprints; [RFC 8827](https://www.rfc-editor.org/rfc/rfc8827) describes out-of-band WebRTC peer verification. The pinned [Espressif `esp_peer` v1.3.0 API](https://github.com/espressif/esp-webrtc-solution/blob/v1.3.0/components/esp_peer/include/esp_peer.h) exposes channel reliability/order settings but no public remote-fingerprint verification hook. In the IDF 6 build path, [`dtls_srtp_v6.c`](https://github.com/espressif/esp-webrtc-solution/blob/v1.3.0/components/esp_peer/src/dtls_srtp_v6.c) sets `MBEDTLS_SSL_VERIFY_OPTIONAL`; shared [`dtls_common.h`](https://github.com/espressif/esp-webrtc-solution/blob/v1.3.0/components/esp_peer/src/dtls_common.h) checks mbedTLS verification flags after the client handshake but returns success without rejecting nonzero flags. The private `remote_fingerprint` field is not read or compared by the visible C wrapper. However, the ESP32-S3 peer implementation is linked from a prebuilt `libpeer_default.a`; its SDP parser/fingerprint strings do not establish whether the archive compares the actual DTLS certificate to the SDP fingerprint. Treat this stack as unaccepted for secured control until source-level proof or a wrong-certificate test confirms fail-closed behavior before control DataChannel data or HID enablement; if it cannot be proved, patch/fork the component or choose a backend with an explicit verification path. Secure WSS, revocation persistence behavior, shared Rust/C protocol vectors, and ESP32-to-Rust interoperability still require implementation tests; no secure session gate passed.

## M0 preparation

- Separate durable requirements from implementation sequence.
- Resolve user policy questions in [plan.md](plan.md), or keep dependent work blocked.
- M0 protocol and security contracts are recorded in [design.md](design.md); network-stack measurements and cross-stack interop remain pending.
- Host inventory is captured above. No gate is passed without direct evidence from the board and private LAN.

## Milestones

### M0: Bootstrap and initial feasibility — closed

- Record the initial architecture, trust boundaries, candidate stack, and protocol direction.
- Establish working firmware/client toolchains and representative UART/HID coexistence, Wi-Fi, and basic DataChannel evidence.
- Carry unresolved findings into the implementation feature that owns them.

M0 exit is satisfied under the revised scope. This is a planning handoff, not a security acceptance. Further stand-alone feasibility work is justified only when it answers a specific implementation blocker.

### M1: Build the Windows client and ESP32 firmware together

Implement vertical slices in the product workspace. Reuse the existing bench modules where useful; avoid building a second complete test application before the product. Add tests and telemetry to the code being delivered. Independent work may proceed across slices while a gate is unresolved; a gate blocks its dependent capability, not all development.

#### M1.1: Workspace, USB setup, and device identity

- First firmware usability priority: turn the onboard RGB LED off at the earliest application initialization. Default to off, with no full-brightness startup animation; this must not wait for networking or security integration.
- Create firmware and Rust workspace boundaries plus a minimal usable Windows client for board selection, USB status, and local Wi-Fi setup.
- Add simple USB/client LED controls: off/on, brightness, and RGB color where supported. Save the preference with device settings and restore it on reboot; connection/error status must respect off/brightness preferences. Use the supported hardware profile's LED wiring, not unit-specific identifiers.
- Discover compatible ports/capabilities and validate detected memory/partition bounds. Device identity comes from generated keys and USB enrollment, not sample labels or serial-number allowlists.
- Implement product USB framing, status/provisioning codecs, identity generation, and storage. Add shared C/Rust vectors and parser rejection tests as each command is implemented.
- Keep credentials local and redacted; test ordinary reboot/persistence alongside the storage feature. No LAN control or HID output is enabled by this slice.

Exit: the real client can select a compatible board, display status, provision Wi-Fi, and control supported RGB LED settings over USB. Verify off/default behavior, dim brightness, chosen color, and persistence across reboot/reconnect on representative hardware. LED-off firmware is the first usable increment; customization follows within M1.1. This deliverable does not wait for complete WebRTC/WSS verification.

#### M1.2: Transport implementation and certificate binding

- Integrate ESP32-to-Rust DataChannels and known-payload echo into product transport modules, initially with no HID output. USB SDP exchange is a temporary internal test aid.
- Fix or replace the backend with the recorded fingerprint failure. Keep the unmodified failing backend excluded from secure control; do not weaken certificate verification to obtain a passing connection.
- Add synthetic certificate fixtures and unit/integration tests with the transport: valid A/A and B/B sessions connect, B/A is rejected in each DTLS role, and absent/malformed identities fail closed. A valid baseline must pass before rejection results count.
- Measure setup/reconnect behavior and initial heap/CPU during actual implementation. Build discovery support in parallel; defer service advertisement until a real endpoint exists in M1.3.

Exit: the integrated Windows/ESP32 transport exchanges known payloads, and its exact backend passes certificate identity tests. This gates use of that transport for remote input, not work on the client, USB, storage, discovery, or WSS modules.

#### M1.3: Enrollment, authenticated signaling, and discovery

- Implement USB enrollment, persistent per-client authorization, revoke/revoke-all, pinned HTTPS/WSS, signed session transcripts, and replay/version checks in the product client and firmware.
- Add WSS certificate fixtures for valid pin, different key, SAN, validity, and signature checks as the verifier is implemented. Verify failures occur before HTTP Upgrade or sensitive signaling data.
- Publish DNS-SD for the actual endpoint, consume it in the client, and expose discovery diagnostics. Test Ethernet/Wi-Fi and Surfshark compatibility as part of that feature; discovery failure must not prevent USB setup or unrelated development.
- Test unauthorized and revoked clients, persistence failures, and identity changes alongside the corresponding flows. Serial-only signaling is not acceptance of this slice.

Exit: an enrolled client establishes the pinned/authenticated session through the real discovery/signaling path; invalid identities and unauthorized clients are rejected. Combined M1.2/M1.3 security gates must pass before network-originated input can affect USB HID.

#### M1.4: Scripted HID, ownership, and measured operation

- Add the event decoder, one-controller lease/takeover, and scripted transport/HID modes. Build the same-host HID observer and report schema with the events they measure.
- Before enabling network-to-HID output, pass M1.2/M1.3 plus implemented parser/ownership/held-input tests. Transport-test mode always emits zero HID reports; live physical-input capture remains off.
- Test cleanup on disconnect, timeout, takeover, revoke, and restart as each path is implemented. Use two enrolled client identities on one available board to test ownership; no particular pair of physical boards is required.
- Measure combined HID/Wi-Fi/WSS/WebRTC build size, minimum internal heap, PSRAM, CPU, setup, reconnect, and sustained traffic. Set repeat counts and acceptance limits from this integrated baseline.

Exit: one enrolled Windows client drives scripted HID through the real secure path, a second client demonstrates exclusive takeover, cleanup checks pass, and integrated resource/timing evidence is recorded.

#### M1.5: Recovery and repeatable bring-up

- Implement and test BOOT hold timing, reset erase scope, reboot-only RST, USB recovery, and key-loss re-enrollment with the actual identity/storage implementation.
- Define safe firmware/bootloader recovery and backup procedures. Track the existing sample bootloader SHA finding as a device issue; do not make unrelated client/firmware work wait for that unit.
- Make the same firmware/client bring-up flow work through detected capabilities and newly generated identity. Exercise that flow with the available device and fresh-identity test fixtures. Checks on another physical unit are optional sampling when available or when a hardware difference is suspected, never a third-board prerequisite.

M1 exit: the implemented vertical slice passes its security, scripted HID, ownership, recovery, and resource checks. M2 adds live capture, hotkeys, switching, and emergency escape with their own tests before live input is enabled.

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

### M5: Extended hardening and portability

- Expand the security coverage built through M1/M2: threat-model review, parser fuzzing, and wider downgrade/replay/identity regression cases. Security does not wait until M5.
- Package Windows client and firmware update/recovery paths.
- Evaluate Linux/macOS support without weakening Windows-first behavior.


## Acceptance checklist

Implement report schemas and repeatable tests with their M1 features; stabilize them as those slices close. Fixed latency targets follow the integrated measured baseline. Apply session/scripted/HID checks in M1; complete live capture, target-switching, and emergency-escape checks in M2 before enabling live input.

- **Enrollment/identity:** two clients enroll independently over direct USB; LAN discovery cannot enroll a client; an unexpected endpoint identity is rejected.
- **Authorization:** unregistered/revoked clients cannot obtain control. Enrolled LAN clients cannot mutate the authorization list; enrollment, per-client revoke, and revoke-all require direct USB. USB revocation of active control releases held input and invalidates its lease. Restart never silently restores revoked access.
- **Transport mode:** deterministic events use the same authentication, WSS signaling, WebRTC, CBOR, and ESP32 decoder as live mode; no USB HID is emitted. Compare sequence numbers, normalized reports, drop/reorder counts, and queue telemetry.
- **HID bench mode:** observer filters by ESP32 Raw Input device identity; expected key/modifier/button/wheel/relative-motion reports arrive without suppressing physical devices.
- **Ownership:** exactly one active control lease; old lease invalid before new owner emits input. Test explicit rejection and no-response takeover after 10 seconds. Implement the documented single-pending-request policy and its race tests together in M1.4.
- **Held-input safety:** switch, disconnect, timeout, revoke, endpoint restart, and emergency escape clear or reconcile held keys/buttons. Define heartbeat/lease expiry and maximum cleanup delay before implementation.
- **Selected-target sessions:** switching releases the old lease and closes its network session before connecting the new target. Inactive targets retain no warm WebRTC/signaling sessions. USB HID remains attached. Measure new-session setup and switch latency on the LAN; do not assume it meets an unmeasured target.
- **Reconnect:** test endpoint restart, address change, network loss, and Windows sleep/resume. Revalidate identity, authorization, capabilities, and ownership before input resumes. Recover automatically to the same selected target only while the outage has not exceeded 60 seconds; discard outage input. Test recovery just before the deadline and failure to recover beyond it, including sleep lasting over one minute. Partial handshakes must not reset the deadline. After timeout, pending connections close, retries stop, input stays disarmed, and late completions cannot resume it. Explicit target selection/connection through the app rearms using saved enrollment. Manual disconnect/disarm/escape cancels recovery immediately. Never silently select another target.
- **Latency/resources:** record transport RTT, board-local processing/queue time, same-host send-to-HID receipt, CPU, flash, PSRAM, and internal-heap use during TLS/WSS setup and sustained traffic. Report one-way timing only with clock mapping and uncertainty.
- **Recovery:** verify deliberate BOOT factory reset, RST reboot-only behavior, COM recovery, and key-loss USB re-enrollment. Document agreed erase scope and recovery authorization.

M1 implementation establishes repeat counts, cleanup deadlines, resource headroom, and pass/fail limits from the integrated behavior. M1 cannot close with these fields unresolved; they are not prerequisites for starting it.

## Evidence record for each execution gate

Record results here as work proceeds:

- Date, board/module, firmware/client revision, tool/library versions.
- USB wiring, target/client roles, OS, and network/VPN configuration.
- Commands or procedure, expected behavior, actual result, and pass/fail.
- Build size, minimum free internal heap, PSRAM use, CPU, connection/reconnect behavior, and timing method/uncertainty.
- Redacted log/report paths; never credentials or private keys.
- Failures, resulting design changes, and next gate allowed to proceed.

Board identity, partition/security state, and USB coexistence evidence for boards 1 and 2 are recorded above. HID-only dynamic heap baselines exist for both, plus a board 2 idle reading with Wi-Fi code linked but not initialized. Wi-Fi association and the board's direct unicast mDNS response are recorded below. Windows .local resolution and multicast discovery remain unresolved; DNS-SD, WebRTC, and full-stack resource measurements remain untested.

### M0 read-only follow-up (2026-11-15 23:20 PST)

A fresh `ipconfig /all` and `route print -4` snapshot shows Surfshark WireGuard active at `10.14.0.2/16`; split default routes `0.0.0.0/1` and `128.0.0.0/1` point through it. Ethernet remains `192.168.50.2/24` with an on-link `192.168.50.0/24` route and multicast route. This confirms host routing only. The board has not joined Wi-Fi, so association, hostname mDNS reachability, and VPN/mDNS compatibility remain untested. `Get-Net*`/CIM queries are denied in this session; read-only `ipconfig`/`route` succeeded.

COM6 is present. Existing PowerShell process `16328` remains responsive with a window titled `kvmctl M0 Wi-Fi setup`; no duplicate form was launched and no serial command was sent during this check. No credentials or Wi-Fi attempt are recorded.

Read-only bootloader follow-up confirmed no safe basis to date or explain Board 2's invalid validation SHA. The 32,768-byte `%TEMP%\kvmctl-m0-board2-bootregion.bin` snapshot has SHA-256 `a16c10210c2d08d89c8b8e0867aa8b38924723de9053813e32adfabba2fe107c`; its extracted 18,704-byte bootloader image has SHA-256 `9fc58b7c1e668368e369d8c4717895098794fde7515e42c1cef0d6cfdb21c962`. `esptool v5.6.0 image-info` calculates `cbd95a816bf0b42be96621cc70ea54ca844ad51f007c6baddbfea1444d8f0913`, matching ROM's calculated digest, while stored digest remains `48304bcd6dd6bd250bc1ad011a0549e9b13e819f8aa98e34f1df27c016a39ae9`. No pre-M0 bootloader backup or manufacturer-matched reference exists. Readback is not a pre-flash backup; do not write a replacement without a safe diagnosis.

No new hardware or secure-stack gate passed. Full-stack heap/CPU/traffic measurements remain absent, so numeric resource limits remain unset rather than inferred from HID-only or Wi-Fi-uninitialized readings.

### Board 2 Wi-Fi attempt: reason 201 (2026-11-15)

After local credential entry, Board 2 reported disconnect reason `201`. The ESP-IDF v6.1.1 `wifi_err_reason_t` enum defines `201` as `WIFI_REASON_NO_AP_FOUND`, `202` as `WIFI_REASON_AUTH_FAIL`, and `204` as `WIFI_REASON_HANDSHAKE_TIMEOUT`; probe `STATUS` prints the disconnect event's reason verbatim. This points to AP discovery, not a direct password rejection. The user reports a phone connects to the visible `whisketeers-iot` SSID with the same password and confirms 2.4 GHz. The Windows flyout shows the secured SSID but not channel or RSSI. The form clears its SSID field after sending, so exact transmitted spelling/whitespace is not retained.

ESP-IDF v6.1.1 default country settings use world-safe `01`, channels 1–11 active and 12–14 passive in station mode; a hidden SSID on passive-only channels can be missed. See [ESP-IDF Wi-Fi driver overview](https://docs.espressif.com/projects/esp-idf/en/v6.1.1/esp32/api-guides/wifi-driver/overview.html). The image permits one association attempt per boot. The prior attempt has not been repeated. A board-side active scan added to the temporary probe now confirms that the exact visible SSID is detectable; the scan does not prove association or credentials. No IP or mDNS result exists, so Wi-Fi and discovery gates remain open.

### Board 2 board-side SSID scan (2026-11-16)

Built with ESP-IDF v6.1.1 as `d51a3d0-dirty`; app image size 858,592 bytes, SHA-256 `d53b1a38807192ff296b5eaf49fc9fa9f895f89d54a9f195007007e6e7e93598`. `esptool v5.6.0 image-info` reported a valid ESP32-S3 image checksum and validation hash. Wrote only this app at offset `0x10000`; a readback of 858,592 bytes matched the image SHA-256. Bootloader and partition table were not written.

Sent `SCAN 776869736b6574656572732d696f74` over CH343 COM6. The firmware performed an active SSID-filtered scan and returned one match: channel 2, RSSI -24 dBm, WPA2. No password was sent. This confirms Board 2 can currently see the SSID with strong signal. No association retry was made; IP assignment, hostname mDNS, and VPN/client reachability remain untested. The M0 Wi-Fi gate stays open.

### Board 2 invalid-length credential check (2026-11-16)

Sent a six-byte dummy WPA passphrase using the bench `WIFI` command. The probe returned `ERR credentials` before station initialization because its validation requires 8–64 bytes for a non-empty password. Follow-up `STATUS` remained `wifi=disabled ip=0.0.0.0 mdns=off reason=0`. The AP was not contacted, so this was not an authentication failure and did not consume the one association attempt for this boot. A radio-level wrong-password test needs a syntactically valid 8–63-byte passphrase (or 64 hexadecimal characters); no substitute was sent. Wi-Fi association and mDNS gates remain open.
### Board 2 valid-length wrong-password association test (2026-11-16)

Sent a syntactically valid, deliberately incorrect test passphrase over CH343 COM6 using the `WIFI` command. The board completed AP discovery and reported `wifi=failed ip=0.0.0.0 mdns=off reason=15`. ESP-IDF v6.1 station-scenario documentation identifies reason 15 as `WIFI_REASON_4WAY_HANDSHAKE_TIMEOUT` and lists it as a wrong-password outcome for an encrypted AP. This matches the expected negative authentication test; the earlier scan found one WPA2 match on channel 2 at -24 dBm. No DHCP address or mDNS initialization occurred. The association attempt for this boot is consumed; successful association remains untested. See [ESP-IDF station scenarios](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-guides/wifi-driver/station-scenarios.html).
### Board 2 real Wi-Fi association and mDNS checks (2026-11-16)

Following local credential entry, Board 2 reported `wifi=connected ip=192.168.50.137 mdns=on reason=0`. Host Ethernet is `192.168.50.2/24`; Surfshark WireGuard remains active at `10.14.0.2`. The route table has an on-link `192.168.50.0/24` route via Ethernet and a multicast route via `192.168.50.2`. ICMP from the PC to `192.168.50.137` replied in 2 ms.

The configured hostname is `kvmctl-m0-probe.local`. A direct unicast mDNS A query from `192.168.50.2` to `192.168.50.137:5353` returned one answer containing `192.168.50.137`. However, Windows `Dns.GetHostAddresses()` and `ping kvmctl-m0-probe.local` could not resolve it. Repeated direct mDNS multicast queries to `224.0.0.251:5353` received no response, including probes with a UDP 5353 listener joined to the multicast group and egress explicitly selected to Ethernet (QM and QU questions). The board's mDNS responder is reachable by unicast, but Windows hostname resolution and multicast discovery across this host's LAN/VPN path are not verified. Firmware publishes no DNS-SD service. Keep the M0 discovery gate open.
### Board 2 connected Wi-Fi idle heap snapshot (2026-11-16)

With Wi-Fi connected and hostname mDNS initialized, a UART `PING` emitted heap phase `uart-line`: internal free 269,139 bytes, minimum internal free 235,016 bytes, PSRAM free 8,385,496 of 8,388,608 bytes. This is an idle connected-board snapshot; no WSS/WebRTC or sustained traffic was active, and CPU was not measured. It is not the full-stack resource gate.
### Board 2 connected LAN latency baseline (2026-11-16)

With Wi-Fi connected and Surfshark active, 20 ICMP probes (32-byte payload) from Ethernet `192.168.50.2` to Board 2 `192.168.50.137` returned 20/20 (0% loss), minimum 32 ms, maximum 128 ms, average 75 ms. A separate initial probe was 2 ms; the 20-packet sample is the recorded baseline. ICMP is only a LAN reachability measure, not app-level or WebRTC sustained traffic.

### Rust-only WebRTC checks (2026-11-16)

In `%TEMP%\webrtc-rs-0.21.1`, `cargo test --test data_channels_interop -- --nocapture` passed 1 test: Rust `webrtc` and Rust `rtc` peers opened a DataChannel, exchanged, and echoed a test string. The test attempted STUN at `74.125.250.129:19302`, which logged network-unreachable; the local peer connection still succeeded using local candidates. This is not ESP32/Rust interop or Internet ICE evidence. `cargo test --test remote_certificate_fingerprint -- --nocapture` passed 1 test that cross-checks Rust local/remote certificate fingerprints from stats; it does not test a mismatched fingerprint rejection. No WSS pinning or ESP32 `esp_peer` fail-closed gate was exercised.
The local Rust checkout also contains `%TEMP%\webrtc-rs-0.21.1\rtc\tests\dtls_disable_fingerprint_verification.rs`, which constructs a mismatched SDP fingerprint and asserts default rejection; a separate test asserts that explicitly disabling verification accepts it. Execution was attempted but could not build: the online run could not reach crates.io, and offline mode lacked the `assert_matches` dev-dependency. This is Rust `rtc` reference evidence only, not a result for the ESP32 `esp_peer` archive.

### M0 blocker review (2026-11-16)

Read-only follow-up at repository revision `7490db6`: Board 2 still answered two ICMP requests at `192.168.50.137` (77 and 79 ms) with Surfshark active. A DNS A query for `kvmctl-m0-probe.local` returned a 55-byte unicast reply from the board with one answer, `192.168.50.137`. Separate multicast QU and QM queries to `224.0.0.251:5353`, with Ethernet selected as multicast egress and group membership on `192.168.50.2`, received no board response within 2.5 seconds each. The QM listener used shared UDP port 5353. This reproduces the discovery failure but does not identify whether host filtering/VPN, router forwarding, or the board receive path causes it. No firmware, credentials, network settings, or serial state were changed.

Source review confirms the current probe cannot exercise the remaining secure-stack gates: `start_mdns()` sets only a hostname; no DNS-SD service, HTTPS/WSS listener, or WebRTC peer exists. The two review tasks independently checked firmware and temporary stack artifacts. Repeating Wi-Fi association, Rust loopback, or Rust certificate-stat tests cannot supply the missing ESP32 evidence.

The following was the bench-only sequence proposed before the planning revision. It is retained as history; the current M1 slices above supersede its global ordering:

1. First use the existing upstream two-peer S3 demo for a smaller backend rejection test: isolate a no-media bench variant, establish a valid DataChannel baseline, then alter the forwarded SDP SHA-256 fingerprint in each direction while leaving the presented DTLS certificate unchanged. Its `peer_msg_handler()` forwards SDP directly between peers, providing an injection point. Require rejection before DataChannel open and zero application bytes. The upstream SoftAP/audio defaults are not the desired bench configuration. Record checkout tag `v1.3.0`, resolved `esp_peer` component `1.5.3`, and ESP-IDF `6.1.1` separately. Visible DTLS code and archive symbols do not establish fail-closed behavior; reject or replace the stack if this runtime check fails.
2. If the backend passes, build a separate no-HID-report ESP32/Rust interoperability harness with the exact S3 archive and pinned client version. Exchange SDP over local serial for this isolated transport check, restrict ICE to the private LAN, and require a valid DataChannel message/echo baseline plus wrong-fingerprint rejection in each DTLS role. Serial signaling is a bench aid, not acceptance of the product WSS path. Same-board ESP peers alone do not establish Windows interoperability.
3. Isolate multicast failure with packet observation or a controlled second LAN host. Distinguish query egress, board receipt/reply, and Windows delivery before changing VPN/router settings. Add DNS-SD together with a real bench endpoint; hostname-only mDNS cannot pass service discovery.
4. The earlier proposal deferred pinned/authenticated HTTPS/WSS and combined resources until after the transport gate. The revised plan instead builds those modules alongside transport, attaches their own tests, and retains the sample bootloader finding in M1.5.

Firmware leaves the ESP-IDF default modem power-save setting unchanged. A controlled `WIFI_PS_NONE` comparison can investigate the recorded idle ICMP latency; current latency is not proof of a WebRTC limit. The one-association-attempt-per-boot restriction is intentional. The setup form discards non-STATUS serial replies, so errors such as `ERR wifi_already_attempted` are not displayed; this is a bench usability limitation, not evidence that the successful connection failed.

### Reproducible board security bench (2026-11-16)

Added [peer-fingerprint](../verification/m0/peer-fingerprint/README.md), a no-media/no-HID two-peer S3 probe, and [mdns-probe.ps1](../verification/m0/mdns-probe.ps1). Source revision for the selected peer checkout is `3971094e2e7c2ab7cb4de59e4eae710e1902d2b4`; the build rejects implementation changes. The runner validates Board 2's factory partition, backs up the complete `0x1F0000`-byte slot at `0x10000`, and restores/verifies it after each test. The original current-app slot backup SHA-256 is `a81d415a34f955d440a166558dce5f1769001b08f5a75bcb2d605ae5e8e55718`; local evidence is under `verification/m0/peer-fingerprint/logs/`. Bootloader and partition table are not written. Restoring the app does not restore RAM-only Wi-Fi credentials.

Initial bench corrections: use a normal EN reset instead of the ROM-download reset helper, and put certificate generation in a dedicated 24 KiB task because it overflowed the default 3.5 KiB main-task stack. These were harness failures, not fingerprint results. Corrected runner detects panics and always attempts verified app restoration.

Run `20261116-101256` (image SHA-256 `4327cafe0f40712e47aae6f2cdfee21ef5f3f4a198b7305c7963e467e744bf59`) paired both local ICE peers and entered DTLS client/server roles, but opened no channel and exchanged no payload. Baseline therefore remained inconclusive and mismatch cases were skipped. Internal heap after peer creation was 129,811 bytes; minimum reached 84,496 bytes; PSRAM free was 7,566,252 bytes. This excludes native HID, WSS, external peers, CPU measurement, and sustained traffic. It cannot pass the full-stack resource gate.

That run used a too-short `agent_recv_timeout=20` ms. Archive disassembly shows its DTLS receive adapter retries 11 empty agent reads, logs `agent_recv timeout`, then returns the literal `-0x7880` (named `MBEDTLS_ERR_SSL_PEER_CLOSE_NOTIFY`). Thus the log is not proof of an actual peer close alert. The roughly 220 ms receive budget can expire before the configured 1-second DTLS initial retransmit. Next baseline uses 200 ms per agent read and records media/MID/SCTP SDP lines without ICE credentials. No fingerprint enforcement verdict exists yet.

Discovery follow-up: multicast QU/QM and separate-group local multicast loopback also timed out when the probe ran outside the sandbox. Read-only firewall inspection found Public-profile Ethernet, a Private-only PowerShell UDP allowance, and a Public inbound block for an existing Rust test binary. These are possible host-side obstacles, not a proven causal diagnosis. Packet monitor access is denied even outside the sandbox; no trace/filter or firewall change was made. User-supplied Surfshark screenshot shows **Invisible on LAN off** while VPN is connected, ruling out that toggle as currently enabled. Other filtering remains unisolated.

### Selected backend fingerprint gate failed (2026-11-16)

Run `20261116-101724` used the sufficient receive-timeout margin and completed all three cases on Board 2. Valid baseline opened both DataChannels and each peer received 10 known payloads. Changing the remote SDP fingerprint in either direction, while keeping certificates unchanged, also opened both channels and delivered 10 payloads each way. Each negative case confirmed one fingerprint mutation and zero harness signaling errors. The selected unmodified S3 backend therefore **fails** the required SDP-to-certificate identity binding; this is no longer merely an unproved property.

| Case | Mutation | Channels opened | Payloads received | Result |
| --- | --- | --- | --- | --- |
| Baseline | None | Both | 10 / 10 | Transport baseline passed |
| Source 0 SDP | One SHA-256 fingerprint changed | Both | 10 / 10 | Security failure; receiving peer is DTLS client |
| Source 1 SDP | One SHA-256 fingerprint changed | Both | 10 / 10 | Security failure; receiving peer is DTLS server |

Minimum internal heap reached 84,340 bytes across the three two-peer cases; internal free returned to 248,011 bytes after peers closed. These remain isolated SoftAP/two-peer measurements without HID/WSS/Windows/CPU/sustained load. Runner restored and verified the original complete factory-app backup. Raw redacted UART evidence is local at `verification/m0/peer-fingerprint/logs/20261116-101724/uart.log`; the same directory records the exact probe-image SHA-256.

Unmodified `esp_peer` 1.5.3 from this pinned checkout/IDF combination is rejected for secure control. A separately identified source patch must pass valid-certificate and mismatch checks before reconsideration; do not describe a patched result as acceptance of the upstream binary unchanged. This finding now belongs to M1.2; M0 bootstrap closure does not accept the failing backend.

### Patched candidate and interop preparation (2026-11-16)

Source hardening patch `verification/m0/peer-fingerprint/esp-peer-fingerprint.patch` adds strict SDP fingerprint parsing and required DTLS leaf-certificate matching in both roles. Patch SHA-256 is `d238d5cd9d5e870ec1d2e87510282e7a1c14a2eaaf917df96e3baec66224ce47`. It builds from an isolated export; linked firmware contains the parser, registry lookup, and verification callback. Extracted-C parser checks passed 1,306 cases. Independent review found no initial-handshake binding blocker; product callers must still disarm and close a session on subsequent SDP errors after callbacks unwind.

Board run `20261116-102913` did not establish a valid patched DataChannel baseline, so negative cases were not run and the patch is not accepted. The original complete app slot was restored and verified against the recorded backup hash. Do not count a built patch or parser checks as a passed certificate gate.

A separate Rust peer and USB bridge in `verification/m0/rust-peer` build offline; boundary/framing checks pass. ESP32 serial-peer module syntax checks pass, but firmware integration and actual ESP32-to-Windows traffic are untested. No new LAN interop result exists.

The [certificate-fixture test design](../verification/m0/peer-fingerprint/certificate-tests.md) specifies ordinary unit and integration cases with synthetic certificates A/B, expected matching/mismatching identities, and explicit pass/fail evidence. DTLS fingerprints cover the full certificate; WSS pins cover SPKI and additionally enforce SAN, validity, and handshake signatures. Fixture integration cases remain unimplemented/unrun. This preserves the required rejection coverage rather than substituting naming changes for verification.
