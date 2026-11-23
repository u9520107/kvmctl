# Test and hardware evidence

Dated test results and hardware observations for the [execution plan](execution-plan.md). Older snapshots are retained as history; later records supersede them where states differ.

## Earlier current-evidence snapshot

| Item | Status | Evidence / next check |
| --- | --- | --- |
| Private local Wi-Fi separate from hotel network | Set up, user reported | Reported 2026-11-15; topology and isolation not independently tested |
| ESP32-S3 hardware configuration | Representative samples identified; bootstrap sufficient | N16R8 samples report ESP32-S3 rev 0.2, 16 MB flash, and 8 MB PSRAM, with CH343 UART and native USB. Product targets compatible hardware capabilities, not these specific units; no batch-wide qualification gate. |
| COM/native USB topology | Boards 1 and 2: CH343 UART0 data + native HID coexistence confirmed | CH343 bridges (`1A86:55D3`, COM3/COM6) remain active while native USB enumerates keyboard and mouse HID (`303A:4004`). The repository M0 probe returned a 4-byte count for `PING` on both. Native USB CDC (`303A:4001`) and ROM USB-Serial/JTAG (`303A:1001`) were also observed in their respective modes. Internal routing details, pin-powered modes, and CDC+HID on one USB connection remain untested. |
| Sample bootloader/RST finding | Device issue retained; product recovery work in M1.5 | Probe recovered after RST, but this sample's existing bootloader reports a SHA mismatch. Preserve backups/evidence and use a safe recovery procedure; this does not block unrelated product implementation. |
| Board Wi-Fi provisioning | Board 2 associated and received DHCP address | Local credential entry connected Board 2 at `192.168.50.137`; ICMP succeeds from Ethernet with Surfshark active. Board 1 remains on the no-Wi-Fi probe. Credentials remain RAM-only. See dated evidence below. |
| mDNS, VPN compatibility, HTTPS/WSS, WebRTC | Multicast unresolved; M1.2 closed | Product firmware and the Rust client exchanged known payloads over USB-signaled DataChannels with fingerprint enforcement. DNS-SD and authenticated WSS remain M1.3 work. |
| Stack documentation review | Unmodified ESP32 backend rejected; patched backend verified | ESP-IDF C + `esp-webrtc-solution` commit `3971094e2e7c2ab7cb4de59e4eae710e1902d2b4` / `esp_peer` 1.5.3 with recorded fingerprint patch; Windows Rust + `webrtc-rs` 0.21.1. The isolated firmware probe rejected mismatches in both DTLS roles; product cross-stack evidence is below. Product heap and peer-task CPU were sampled in transport-test mode; total system CPU and sustained-load resources remain unmeasured. See [design](design.md). |
| Firmware/client implementation | M1.1 and M1.2 closed | App-only product images preserved Board 2 identity, settings, and LED behavior. Product transport tests passed both channels, reconnect, invalidation, certificate cases, and resource sampling. Portable bridge-marker provisioning is deferred. |

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

The initial source hardening patch `verification/m0/peer-fingerprint/esp-peer-fingerprint.patch` adds strict SDP fingerprint parsing and required DTLS leaf-certificate matching in both roles. Its first candidate hash was `d238d5cd9d5e870ec1d2e87510282e7a1c14a2eaaf917df96e3baec66224ce47`; the final candidate and tested behavior are recorded below. It builds from an isolated export; linked firmware contains the parser, registry lookup, and verification callback. Extracted-C parser checks passed 1,306 cases. Independent review found no initial-handshake binding blocker; product callers must still disarm and close a session on subsequent SDP errors after callbacks unwind.

The first patched board run, `20261116-102913`, did not establish a valid DataChannel baseline. That result was inconclusive and is superseded by the later verified run below. The original complete app slot was restored and verified against the recorded backup hash.

A separate Rust peer and USB bridge in `verification/m0/rust-peer` build offline. Product firmware/client transport integration and actual ESP32-to-Windows traffic status are recorded below.

The [certificate-fixture test design](../verification/m0/peer-fingerprint/certificate-tests.md) specifies unit and integration cases with synthetic certificates A/B, expected matching/mismatching identities, and explicit pass/fail evidence. DTLS fingerprints cover the full certificate; WSS pins cover SPKI and additionally enforce SAN, validity, and handshake signatures. Product DTLS fixture cases are recorded below; WSS certificate validation remains M1.3 work.

### M1.3 implementation progress (2026-11-17; open)

Updated [execution-plan.md](execution-plan.md) and [design.md](design.md) acceptance
with the review findings. Firmware implements an NVS-backed store for 8 independent
P-256 client public keys, USB enroll/list/revoke/revoke-all, active-session
invalidation, signed challenge/transcript proofs, replay/version checks, pinned
HTTPS/WSS identity, DNS-SD advertisement, competing-offer busy rejection, and an
application gate that requires client authorization, both transcript proofs, open
DTLS channels, and peer identity checks. TLS uses a two-socket cap, a 4-token
ClientHello-stage bucket with one token refilled every 10 seconds, a 5-second TLS
deadline, a 15-second proof deadline, and 16-KiB message reassembly bound. The
certificate-selection callback enforces the handshake bucket before certificate
selection. USB metrics expose bounded TLS diagnostics without printing onto the
framed administration UART.

The Windows client uses DPAPI-protected independent credentials, persistence/reload,
pinned TLS SPKI/SAN/validity/self-signature checks, signed signaling, Windows DNS-SD
browse/resolve, identity matching to USB status, and `signaling-test` scenarios for
success, missing/invalid/replayed proofs, session isolation, bounded resources, and
active revocation. Negative proof scenarios send application data before authorization
and require zero accepted bytes and no echo. Raw USB `transport-test` sends no
application payloads. `cargo test --workspace --offline --target
x86_64-pc-windows-msvc` passed all 33 tests; `cargo fmt --all -- --check` passed.
Coverage includes shared C/Rust vectors, TLS pin/SAN/expiry/signature rejection,
DPAPI persistence/reload, and DNS-SD identity/address parsing. An independent
firmware-guard review run passed; a local rerun could not compile because Espressif's
clang lacks an x86_64 MSVC target and no MSVC C compiler is installed.

ESP-IDF 6.1.1 built ESP32-S3 N16R8 image `firmware/build/kvmctl_firmware.bin` at
1,245,920 bytes (0x1302e0), leaving 19% of the 0x177000-byte factory app partition
free. COM6 app-only backup/flash/verify succeeded at factory app offset 0x10000;
bootloader, partition table, and NVS were not flashed. USB status reports Wi-Fi
connected, endpoint fingerprint `e299faff8cb8d99166647efa72a1a2354a4b5271c8f3369e70ef584088926fe0`,
and TLS SPKI hash `de97023d354e8a2bb7daa7d0ebd5fac8625fa5710cc9a6b27256c0e9e55ff56d`.
USB enrollment and client listing succeeded from two separate temporary DPAPI
profiles, `%TEMP%\kvmctl-m13-profile` and
`%TEMP%\kvmctl-m13-client-b-profile`, with independent fingerprints
`d0a144fed5cf59ec8e809a1bb24600082b4df74d1a9aadb6f524b11069c34929` and
`02cf5f66a59fe3917d4b816b11376bb6051a6eefbf7157d9326a1712dc3b1129`. Both clients
were still listed after separate `cargo run` processes. The managed sandbox could not
write the user's normal `%LOCALAPPDATA%` profile, so this does not verify that profile
or another Windows account. USB per-client revoke and revoke-all both succeeded;
after revoke-all, listing returned no clients. The two test clients were then
re-enrolled and are the current device list.

`cargo run --offline -p kvmctl -- discover COM6` found no `_kvmctl._tcp.local`
service. Direct IP `192.168.50.137` is reachable over Ethernet with Surfshark
active, and TCP port 443 accepts connections. The first live TLS attempts exposed
an ESP-IDF certificate-selection callback contract: it must attach the certificate
and private key to each handshake. Firmware parses the existing generated
certificate, wraps the same PSA key, and attaches both in the rate-limit callback.
The initial TLS 1.2 handshake still failed. A read-only GPT-6.1 review traced that
failure to the TLS private key's PSA algorithm policy: ESP-IDF enables deterministic
ECDSA, while the TLS key had been imported for randomized ECDSA. Firmware now
imports the same persistent TLS key with
`MBEDTLS_PK_ALG_ECDSA(PSA_ALG_SHA_256)`; endpoint identity signing policy, key bytes,
certificate, and SPKI pin are unchanged. Startup checks that the TLS key policy is
compatible with Mbed TLS. A clean ESP-IDF 6.1.1 rebuild was app-only flashed and
verified on COM6; bootloader, partition table, and NVS were not flashed.

Using a Python TLS 1.2 probe against the flashed board, the full SAN hostname
`kvmctl-e299faff8cb8d991.local` completed TLS with
`ECDHE-ECDSA-AES128-GCM-SHA256`; the TLS stack verified the board certificate when
explicitly trusted as a root. The same certificate was rejected for
`wrong-host.invalid`. A WSS request to `/v1/ws` with subprotocol `kvmctl.v1` returned
`101 Switching Protocols`. This validates the board's TLS handshake, certificate
hostname check, and HTTP Upgrade, but does not exercise the Rust client's SPKI pin
validation or client authorization/transcript proofs. The probe closed the WSS
socket after Upgrade; a subsequent direct USB status query on COM6 returned its
146-byte response. No WebRTC/DTLS session was established, so application gating,
session isolation with an active peer, and negative proof acceptance remain
unverified on hardware.

The final rebuilt image SHA-256 is
`13c5eae7cd79aeaaefd29f18e43e2163aee7101b3613a7c795a86f2cf8f569a5`. Before this
flash, COM6's full 0x177000-byte app partition was saved as
`firmware/backups/m1.3-before-final-com6-20261117.bin`, SHA-256
`8cdc8cdcc167c2414659d64cd73a32bdf479ce2fa33aaab1a06c493753061f3f`. esptool
verified the written image hash and hard-reset the board. A fresh post-flash
Python probe accepted `kvmctl-e299faff8cb8d991.local` under TLS 1.2, rejected
`wrong-host.invalid` (hostname verification error 62), and received
`HTTP/1.1 101 Switching Protocols` with `kvmctl.v1`. This raw TLS/WSS handshake
did not exercise the Rust client's pinning or authenticated signaling. USB status
and WebRTC/DTLS were not checked after this final flash.

### Latest live acceptance run (2026-11-17)

This section supersedes the earlier progress notes above where Rust-client pinning,
authenticated WSS/DTLS, active revocation, and combined-heap evidence were still
pending.

The current app-only image is 1,246,128 bytes, SHA-256
`3af2d2a90af7f9e40aef5cc693dc3c57a8e12b4ce8916e89807e18d8b134b84a`; it leaves
19% of the 0x177000-byte app partition free. It was flashed and verified on COM6;
bootloader, partition table, and NVS were not written. The generated config enables
`CONFIG_HTTPD_WS_POST_HANDSHAKE_CB_SUPPORT`. This callback was required because the
pinned ESP-IDF HTTP server completes a WebSocket Upgrade without calling the normal
URI handler; the callback now initializes bounded session buffers before the first
frame. The Rust client and firmware also use separate endpoint-challenge and
client-proof signing domains, preventing signature reflection.

Using `cargo run --offline -p kvmctl -- signaling-test ...` against direct board IP
`192.168.50.137`, the Rust client verified the pinned WSS certificate and completed
real TLS/WSS, SDP, DTLS, transcript authorization, and DataChannel exchange. The
device identity remained `e299faff8cb8d99166647efa72a1a2354a4b5271c8f3369e70ef584088926fe0`
with TLS SPKI pin
`de97023d354e8a2bb7daa7d0ebd5fac8625fa5710cc9a6b27256c0e9e55ff56d`. Success
reported `AUTHORIZED client_proof=valid endpoint_transcript=verified
dtls_identity=verified`. While client A remained active, an unenrolled client was
rejected, an enrolled client with an invalid challenge proof was rejected, and a
competing authenticated offer returned `busy`; A continued echoing on both channels
after each attempt.

The `missing`, `invalid`, and `replay` scenarios each established DTLS, sent
application payloads before proof, and observed no echo and zero accepted application
bytes. Invalid and replayed proofs were rejected as unauthorized. The `revoke-active`
scenario authorized client B, exchanged payloads, revoked B over USB during the live
session, observed the device return to idle and stop echoing post-revocation payloads,
then confirmed B's next WSS login was unauthorized. USB `revoke all` returned an
empty client list; the two independent test clients were then re-enrolled and are
currently listed. HID output and physical-input capture remained disabled.

The `resources` scenario kept an authorized session alive while a stalled TLS socket
occupied the second slot, verified the socket cap and handshake deadline, applied a
rapid unauthorized-handshake flood until the firmware rate limiter advanced, rejected
an oversized fragmented WSS message, and verified recovery plus continued USB
administration. Its combined WSS/WebRTC sample measured internal heap free
167,179/52,039/141,803 bytes (start/minimum/after) and PSRAM free
8,350,752/7,915,548/8,350,752 bytes. The success run's internal minimum was 56,595
bytes; PSRAM minimum was 7,915,548 bytes. These establish initial caps for the
tested bounded scenarios, not sustained-load limits; broader load work remains M1.4.

Windows DNS-SD browse returns no `_kvmctl._tcp.local` services after a five-second
window, both with all interfaces and with explicit Ethernet interface index 12.
`Resolve-DnsName` also
returns no A record. A direct unicast PTR query from host Ethernet `192.168.50.2` to
the board's `192.168.50.137:5353` did receive and decode the service records:
`KVMCTL._kvmctl._tcp.local`, SRV `kvmctl-e299faff8cb8d991.local:443`, TXT `v=1`,
`path=/v1/ws`, the expected endpoint fingerprint, and A `192.168.50.137`. This
proves the firmware publishes the correct DNS-SD data and answers unicast mDNS.

The Windows DNS Client service has enabled inbound mDNS UDP 5353 rules, but the
Windows browse still sees no record. The raw multicast probe used a Python process;
Windows' inbound mDNS rules are scoped to `svchost.exe`/`dnscache`, so that probe is
not evidence that the firmware failed to answer multicast. The remaining issue is
multicast discovery delivery/consumption on this network. The user confirms the PC's
LAN Ethernet is connected to the network serving the board; it uses
`192.168.50.2/24`, reaches the board, and returns the unicast DNS-SD records. Windows
browse still sees no record on that Ethernet interface. The PC's separate Wi-Fi
address `172.16.0.228` is on another subnet and is not needed for this test; a
source-bound Wi-Fi TCP/443 probe to the board failed. Direct WSS by IP works over
Ethernet while Surfshark WireGuard is active, but this does not prove the tunneled-VPN
path. Multicast forwarding or Windows DNS-SD consumption remains unresolved.
The PC does not need to switch to Wi-Fi: unicast confirms the board's records once its
IP is known, while automatic DNS-SD service discovery is the remaining multicast-browse
check across the PC Ethernet and board Wi-Fi network path.

A packet-path capture during one Windows DNS-SD browse (2026-11-17) recorded three
`_kvmctl._tcp.local` PTR queries from `10.14.0.2:5353` to `224.0.0.251:5353` on the
WireGuard Tunnel interface (index 44). No matching query appeared on the PC's Ethernet
interface (index 12), and no decoded `_kvmctl._tcp.local` answer from the board appeared
in captured NIC traffic. PktMon text conversion reported three formatting errors; all
three target queries decoded. This places the observed failure before Ethernet
multicast egress; it does not establish why Windows DNS-SD selected the tunnel or how
the board/network handles a query sent over Ethernet. No firewall, VPN, or router
settings changed.

With Surfshark disabled, the user ran `cargo run --offline -p kvmctl -- discover COM6`
against current source (2026-11-17). It returned
`KVMCTL._kvmctl._tcp.local`, hostname `kvmctl-e299faff8cb8d991.local`, address
`192.168.50.137`, port 443, the expected fingerprint, and `identity=matches USB`.
This verifies discovery, identity matching, and IPv4 parsing in one VPN-off run. The
earlier pre-fix executable had printed the reversed address `137.50.168.192`. No fresh
packet capture could be started because PktMon returned `Access is denied`, so interface
egress in the successful run is unknown. Surfshark-on discovery remains unverified.

`cargo fmt --all -- --check` and
`cargo test --workspace --offline --target x86_64-pc-windows-msvc` passed (33 tests).
The independent firmware guard harness passed in reviewer execution; a local rerun
could not build because the installed Espressif clang lacks an x86_64 MSVC target and
no MSVC C compiler is installed. Persistence was exercised in two temporary DPAPI
profiles across multiple processes, but the normal user profile and another Windows
account were not tested. A sample boot log still reports the previously documented
bootloader SHA comparison warning before continuing into the verified firmware.

The M1.3 signaling CLI now accepts a bare IPv4 or scenario plus IPv4 to bypass DNS-SD.
After a successful pinned session, it stores that address beside the selected USB
board identity. If DNS-SD returns no matching service, it prompts before trying the
saved address. The address remains a route hint; saved and manual routes use the same
USB-derived hostname, SPKI pin, and transcript checks. This is the M1.3 signaling CLI,
not the later M1.4 control-session UI.

M1.3 current-scope acceptance closed on 2026-11-23. Cross-user DPAPI behavior is
deferred to M2 because no second Windows account is available. The host-side guard
harness and on-device fail-once revoke-persistence retry/reboot check passed.
Normal-profile credential loading, manual IPv4 signaling, a pinned session routed
through DNS-SD, and saved-IP fallback with Surfshark on have passed; details follow.
Surfshark-on DNS-SD still returns no service, but VPN-specific automatic discovery is
deferred to M2 because physical re-enrollment plus known-IP routing is sufficient for
current use. A prior capture showed browse queries on WireGuard and none on Ethernet,
but the cause is unproven. The successful Surfshark-off browse had no packet capture.
PC Ethernet is the acceptance path; testing the PC's Wi-Fi interface is optional. The
firmware advertisement and record contents are verified by direct unicast mDNS. M1.2
DTLS certificate enforcement remains unchanged.
See [execution-plan.md](execution-plan.md) for the acceptance gate.

### Manual IPv4 signaling and normal-profile credential (2026-11-17)

In the normal Windows profile, `enroll COM6` created client credential
`64df35de642ae1a5e123912b3ba3101abfead24e655ae8efc5ca1826e1d7edd5`. A separate
`cargo run --offline -p kvmctl -- signaling-test COM6 192.168.50.137` process loaded
that credential and completed the manual-IPv4 pinned session. The client used USB-derived
TLS name `kvmctl-e299faff8cb8d991.local`; it reported valid client proof, verified
endpoint transcript and DTLS identity, rejected unauthorized and invalid-proof clients,
and rejected a competing authenticated offer as busy while the active session kept
echoing on both channels. Ten control and ten motion messages echoed in the success
scenario. The run reported zero HID reports and physical-input capture off, then saved
`192.168.50.137` as the board's last-known address. This verifies normal-profile
credential loading across separate command processes and the manual IPv4 path. The
saved-IP prompt was verified in a later Surfshark-on run below.

### Manual IPv4 signaling after persistence-error fix (2026-11-23)

With the real Windows profile and Board 2 on `COM6`, the user ran
`cargo run --offline -p kvmctl -- signaling-test COM6 192.168.50.137`.
The pinned session completed DTLS, valid client proof, endpoint transcript and DTLS
identity checks, unauthorized-client rejection, invalid-client-proof rejection,
competing-offer busy rejection, and active-session preservation. Control and motion
each echoed ten messages. The run reported resource minima, zero HID reports, and
physical-input capture off, then saved `192.168.50.137` successfully. This confirms
the current persistence-error handling on the accepted known-IP path.

### Pinned signaling session through DNS-SD (2026-11-17)

The user ran `cargo run --offline -p kvmctl -- signaling-test COM6` with no manual IP.
The session completed DTLS, client proof, endpoint transcript, DTLS identity, session
isolation, and 10-message control and motion echo checks. It reported zero HID reports,
physical-input capture off, and remembered `192.168.50.137`. No saved-IP prompt appeared.
By the CLI control flow, the no-argument run can reach this success only after DNS-SD
returns a service matching the USB identity; therefore this verifies a complete pinned
DNS-SD signaling session. Surfshark was disabled for the prior successful browse; this
run did not include a packet capture, so its multicast egress interface was not
observed. This supersedes the earlier statement above that a DNS-SD-routed session was
still unverified.

### Surfshark-on saved-IP fallback (2026-11-17)

With Surfshark enabled, the user ran `cargo run --offline -p kvmctl -- signaling-test
COM6`. DNS-SD did not find the board; the user accepted the prompt to try saved IPv4
`192.168.50.137`. The pinned session then completed DTLS, client proof, endpoint
transcript, DTLS identity, session isolation, and 10-message control and motion echo
checks. It reported zero HID reports and physical-input capture off. This verifies the
saved-IP prompt and secure fallback under the Surfshark-on condition. Together with the
successful Surfshark-off DNS-SD session and prior capture, the results implicate VPN
multicast routing, but do not prove its root cause or exact packet path in this run.

### Host-side signaling guard test (2026-11-17)

The user ran `python3 firmware/test_signaling_guards.py` from WSL. It passed:
`stale transport generations cannot close/authorize current peer; deadlines expire;
failed revoke persistence stays blocked and returns error`. The harness compiles
selected production C functions into a temporary shared library with mocked platform
services and injected persistence errors. It verifies the immediate in-memory denial
and retry control flow, but does not exercise ESP-IDF or real NVS storage/reboot
behavior. That on-device failure path remains unverified.

### On-device revoke-persistence retry (2026-11-18)

Built a temporary app image with `KVMCTL_TEST_REVOKE_PERSIST_FAIL_ONCE=1`, which
returns one persistence error before the first per-client revoke reaches NVS. The
test image was flashed to COM6 after a verified app-only backup; NVS, bootloader, and
partition table were untouched. Revoking normal-profile client
`64df35de642ae1a5e123912b3ba3101abfead24e655ae8efc5ca1826e1d7edd5` returned device
error 6, while the subsequent client list omitted it from RAM. Retrying the same
fingerprint succeeded. The normal firmware image was then app-only flashed and
verified, resetting the board without erasing NVS; after reboot, the client list still
omitted the revoked fingerprint and contained the other two clients. This confirms the
pending revoke is durably retryable after immediate in-memory removal. It simulates a
persistence error before NVS access; it does not simulate a physical NVS flash-write
failure. The host shell could not re-enroll the Windows client because it runs under a
sandbox account whose DPAPI context cannot decrypt the user's `%LOCALAPPDATA%` credential.
The user re-enrolled from their Windows session. A subsequent read-only `clients COM6`
query listed the restored fingerprint alongside the other two enrolled clients.

### M1.2 transport implementation and patched backend gate (2026-11-16)

The backend remains Espressif `esp_peer` 1.5.3 from `esp-webrtc-solution`
commit `3971094e2e7c2ab7cb4de59e4eae710e1902d2b4`, with the local patch
`verification/m0/peer-fingerprint/esp-peer-fingerprint.patch` (current SHA-256
`6631fed27750b4cf444c82bdf5afd1eb316674abd8bad97875166660aa5eb8ca`). The
board probe used patch-file SHA-256
`060214cb4e77cae952216eb7e8a23ad9ee0af12b9d9ef1b43b7ba3629e536f57`; patched
`esp_peer.c`, `dtls_srtp_v6.c`, `dtls_common.h`, and `dtls_srtp.h` sources in the
current export match the probe export byte-for-byte. The patch requires a
SHA-256 SDP fingerprint, checks the peer DTLS leaf certificate against it in
both roles, and retains required certificate verification. The unmodified
1.5.3 backend remains prohibited for secure control.

On Board 2, isolated probe run `20261116-202453` used probe image SHA-256
`93b7d72038c8791e9fa57fe0d60bd97951336bcf5df7c01fc07675b5a231429c`. The
valid baseline opened one DataChannel at each peer and exchanged 10 known
messages each way. Mutating source 0's SDP fingerprint produced explicit
`pin_mismatch` rejection in the DTLS client role; mutating source 1's produced
the same explicit rejection in the server role. Both negative cases had zero
channel opens, zero received application messages, and zero reported harness
errors. The runner's `M0 VERIFIED_SUITE PASS` requires the valid baseline,
expected role-specific rejection, zero open/receive/error counts, and matching
offerer/answerer roles. The generic per-case label is `INCONCLUSIVE` because
those negative cases intentionally never reach a DataChannel.

The isolated two-peer probe started with 248,319 bytes free internal heap and
8,385,692 bytes free PSRAM before peer setup. It reached a minimum of 74,340
bytes free internal heap and 7,565,916 bytes free PSRAM during the second
negative case; internal heap after both peers closed was 233,387 bytes. The
valid baseline case reached 84,040 bytes minimum internal heap and had 247,995
bytes free after close. CPU, Windows interoperability, product reconnect, and
sustained-load measurements were not part of this probe. The runner restored
the original full app slot and verified backup SHA-256
`2a26bdfa8e4228f43607cb4baa00e23fb24d86f07d2ee56d6a09e6f5d7bfbbdd`.
Redacted UART evidence is local in
`verification/m0/peer-fingerprint/logs/20261116-202453/uart.log`.

Product transport code is integrated in `firmware/main/transport.c` and
`client/src/transport.rs`. The device is the answerer and echoes bounded
payloads on `control` and `motion`; the Rust client creates reliable ordered
`control` and unordered zero-retransmission `motion`, checks those settings,
tests malformed-SDP active-session invalidation, and exercises a fresh-session
reconnect. A USB status request reports a captured backend fingerprint
rejection to the Windows test client while keeping UART logs suppressed. HID
output and physical-input capture remain disabled. USB SDP is an internal test
aid, not production signaling.

The ESP-IDF 6.1.1 product image is 1,083,856 bytes, SHA-256
`412361f2b331f854dd99890504e5921e7a7d13af9fdac55817f43218e135d65c`. Board 2
was flashed through the app-only USB updater; bootloader, partition table, NVS,
and VFS were not written. The app-partition backup SHA-256 is
`2a26bdfa8e4228f43607cb4baa00e23fb24d86f07d2ee56d6a09e6f5d7bfbbdd`. USB
status after flashing confirmed the board identity and saved settings remained
available. The backend source and patch hashes are recorded above.

Test-only certificate A, B, and renewed A are in
`verification/m0/peer-fingerprint/fixtures`. Renewed A uses A's public key but
has different DER. No fixture key was installed on hardware. On Board 2,
`cargo run -p kvmctl -- transport-test COM6` completed valid fixture A and B
sessions. Each session exchanged 10 payloads on each channel, rejected one
conflicting SDP while connected, closed after callbacks unwound, then rejected
malformed SDP on a fresh session and reconnected cleanly. The certificate
matrix below used firmware image SHA-256
`412361f2b331f854dd99890504e5921e7a7d13af9fdac55817f43218e135d65c` and the
same patched `esp_peer` backend revision/hash recorded above. The final
telemetry image retained that backend and passed a separate payload, channel,
invalidation, reconnect, heap, and peer-task CPU run.

| Fixture | Initial/reconnect open (ms) | Control RTT (µs), initial/reconnect | Motion RTT (µs), initial/reconnect |
| --- | ---: | ---: | ---: |
| A | 1643 / 1646 | 10942–28881 / 11722–31930 | 11051–27075 / 10902–28838 |
| B | 1708 / 1637 | 11198–28539 / 11212–27764 | 11057–23259 / 13148–30408 |

Both channels reported their required delivery settings: control reliable and
ordered; motion unordered with zero retransmissions. These setup values include
ICE gathering, temporary USB SDP exchange, DTLS, and channel open. No COM port,
address, identity, or bench /24 restriction is embedded in product constants.

| Receiving path | Case | Result |
| --- | --- | --- |
| ESP32 | Fixture B presented with fixture A fingerprint | Explicit `ESP32 rejected the remote DTLS certificate fingerprint`; no channel opened and no application echo ran. Valid A/A and B/B baselines passed first. |
| ESP32 | Renewed A presented with old A fingerprint | Same explicit rejection. Renewal has same key and different DER. |
| ESP32 | Missing, malformed, or duplicate offer fingerprint | Each rejected as malformed SDP/certificate identity before a channel opened. |
| Windows | Product ESP certificate presented with fixture A as expected fingerprint | Rust verifier's certificate-mismatch warning was captured without printing backend logs; client reported explicit rejection with `DataChannels opened=0, application messages sent=0`. Board's product certificate matched in preceding baselines. Fixture B was not installed on the board. |
| Windows | Missing, malformed, or duplicate answer fingerprint | Each rejected by the client parser before remote description was applied. |

The pinned Rust WebRTC 0.21.1 stack leaves peer state `Connecting` after its
DTLS verifier rejects a certificate. The test client now captures only
warning/error classification (never raw SDP/ICE or backend log text), recognizes
the verifier's exact certificate mismatch, reports zero opened channels and
application messages, then closes the session. The integrated transport tests
prove the production certificate gate without treating a timeout as a pass.

The Python USB-admin host harness could not run because this environment lacks
the native `cc` compiler it invokes. The ESP-IDF product build did compile the
USB request decoder and transport-status response paths.

The first telemetry image (1,087,024 bytes, SHA-256
`3f2b6011b227c529cfebc98378a6a0b41224d76b4184e116ea2021fd9d0123c9`) was
flashed app-only to Board 2. Its verified full app-partition backup is
`firmware/backups/factory-app-before-resource-metrics.bin`, SHA-256
`0c0ff352f610931b800432f619b578c8c01d3958ff97650d7a435c616afda349`.
That run exchanged 10 messages per channel and reconnected cleanly, but showed
state 4 after requested shutdown. It exposed expected DataChannel close
callbacks being classified as failures and an after-close heap reading taken
before the worker stack was reclaimed. Firmware was corrected to ignore those
expected callbacks, sample after task cleanup, and collect `webrtc_peer` task
runtime using FreeRTOS stats and the 1 MHz ESP Timer clock.

The final image is 1,087,760 bytes, SHA-256
`c66667cbae9a408d8cdd527420c543bfa401bb227741cc38f6cfe5068d8d3798`; it fits
the app partition with 29% free. Board 2 ran it after an app-only update. The
verified full app-partition backup is
`firmware/backups/factory-app-before-close-state-fix.bin`, SHA-256
`55cb39fb1748fdb8242b27ef55f88ffd32b6f3265268e9916d840ad2c41106f9`.
After one transient initial-status framing error, the read-only status retry
succeeded and the following run passed:

| Stage | Open (ms) | Control RTT (µs) | Motion RTT (µs) | Internal free start/min/after (bytes) | PSRAM free start/min/after (bytes) | Opens / received bytes | `webrtc_peer` CPU (µs) |
| --- | ---: | ---: | ---: | --- | --- | ---: | ---: |
| Initial | 1614 | 11669–30298 | 11062–36483 | 225859 / 140407 / 224967 | 8375984 / 7966096 / 8375984 | 2 / 510 | 1182326 |
| Reconnect | 1685 | 12091–36477 | 14346–43543 | 225003 / 139743 / 224967 | 8375984 / 7966096 / 8375984 | 2 / 510 | 1172428 |

Both runs reported idle state 0 after close, exchanged 10 messages per
channel, rejected conflicting SDP and malformed SDP after callbacks unwound,
then reconnected cleanly. Control was reliable and ordered; motion was
unordered with zero retransmissions. CPU values measure the `webrtc_peer`
worker task for each full test session, not total SoC CPU; Wi-Fi/other task CPU
and sustained-load resource behavior remain unmeasured. UART0 startup logs can
transiently precede the binary frame; the host retries only the idempotent
status query and keeps transport/certificate parsing fail-closed.

Product M1.2 payload, channel, setup/reconnect, active-session invalidation,
certificate-gate, integrated heap, and peer-task CPU checks passed. Total SoC
CPU and sustained-load resource behavior remain unmeasured. The earlier
isolated two-peer probe memory figures above are not a substitute for the
integrated firmware baseline.
The command is reproducible from source with
`cargo run -p kvmctl -- transport-test COMx`; use the port reported by
`cargo run -p kvmctl -- ports`. It does not capture physical input or emit HID
reports, and it does not log SDP or ICE secrets.

### M1.1 firmware first slice (2026-11-16)

Product firmware now starts in `firmware/`. Its first `app_main` action initializes the addressable RGB strip and clears it with pinned `espressif/led_strip` 3.0.3. Build passes with ESP-IDF 6.1.1 for ESP32-S3 N16R8; flash size is configured as 16 MB and the latest binary is `0x304e0` bytes. GPIO48 is the default profile, with GPIO38 and no-LED profiles available.

Board 2's live partition table confirmed the factory app slot at `0x10000`, size `0x1F0000`. A fresh full-slot backup matched the previously recorded factory app SHA-256. The product image was written only at `0x10000`; esptool verified its hash, and UART logged `RGB LED initialized off`. Bootloader, partition table, NVS, and other data partitions were not written. Board 2 now runs the product image; the backup remains local at `firmware/backups/board2-app-20261116.bin`.

The user visually confirmed Board 2's addressable RGB LED is off after flashing. This verifies the selected GPIO48 profile on this unit. The DORHEA N16R8 manual identifies the separate red lamp as a power indicator; it does not document software control, so it is not part of M1.1 LED settings unless a schematic confirms a controllable driver.

Firmware bring-up now checks for at least 16 MiB flash and 8 MiB PSRAM, validates every partition end against detected flash size, and compile-checks the product administration UART as UART0/115200. Board 2 boot reported 16 MiB flash, 8 MiB PSRAM, valid partitions, and UART0/115200. A duplicate GPIO setup initially caused an RMT reservation warning; it was removed, and the latest boot cleared the RGB LED without that warning. The latest `0x304e0` image was app-flashed only at `0x10000`; esptool verified its hash. The existing bootloader SHA-256 warning appeared again, then boot continued normally; it remains the recorded device issue and the bootloader was not written.

### M1.1 USB client and settings slice (2026-11-16)

Added a Windows Rust control app with COM selection, USB status, local Wi-Fi provisioning through a masked password field, and RGB on/off, brightness, and color controls. CLI remains available. Product firmware stores Wi-Fi credentials and RGB preferences in separate NVS namespaces, restores them at boot, and suppresses firmware logs before starting binary UART framing. Wi-Fi passphrases are not logged or returned. Shared deterministic-CBOR request vectors cover status, Wi-Fi setup, and RGB settings; status includes persisted RGB values.

The UI enumerates present CH343 USB-UART devices through Windows Plug and Play, shows friendly names and USB locations, and associates a responding board's endpoint fingerprint with its PnP instance. Only previously confirmed mappings are recognized; all unknown CH343 candidates require explicit confirmation before opening, including when only one is present. After a user confirms an unknown candidate, the client requests and validates status, saves its PnP-to-fingerprint mapping, and refreshes the selector label immediately. Cancel exits before opening the port; startup enumeration is metadata-only. The editable SSID dropdown accepts typed hidden SSIDs and now requests an explicit Windows WLAN scan before reading available networks. Current Windows Rust checks pass with `cargo check --workspace --tests --offline --target x86_64-pc-windows-msvc`; tests were type-checked, not executed. Portable bridge-marker provisioning and fresh-PC automatic recognition are deferred.

### M1.1 client retest notes (2026-11-16)

Initial user QA confirmed the unknown-device warning. A screenshot showed the selector label stayed stale after mapping; code now validates status, saves the mapping, and refreshes the selector immediately. LED labels were shortened (`Brightness`, `%`, `RRGGBB`) to prevent wrapping in the fixed-width window.

User decision on 2026-11-16: defer full UI-framework/visual redesign beyond M1.1. Retain Win32 controls and limit this slice to required usability fixes.

A read-only check found the local board-map entry for the connected CH343 instance, confirming the prior Wi-Fi operation had already persisted the identity despite the stale selector label. Fingerprint contents were not printed.

`cargo test --workspace --offline` passed 6 checks; release build passed. ESP-IDF 6.1.1 build passed for ESP32-S3 N16R8; app image is `0xcdd50` bytes (SHA-256 `eb393f444e47d0ef25351a5a39565f936d56a4c6ea92326ebf57e52a9c760793`). The full factory-app backup remains unchanged at SHA-256 `a81d415a34f955d440a166558dce5f1769001b08f5a75bcb2d605ae5e8e55718`. The image was flashed to the factory app slot only; esptool verified written data. Bootloader, partition table, and NVS were not written.

The Windows client queried Board 2 on COM6 and validated its endpoint identity and TLS pin. Endpoint fingerprint remained `e299faff8cb8d99166647efa72a1a2354a4b5271c8f3369e70ef584088926fe0` after app update and software resets. RGB off and on commands succeeded; status reported `on, 12%, #FF2200` after reset. The user then disabled RGB through the control app. After local Wi-Fi provisioning, status reported connected. A further hard reset with `esptool run` did not write flash; the next USB status still reported Wi-Fi connected, RGB off at 12% with color `#FF2200`, and the same identity fingerprint. This verifies Wi-Fi, RGB preferences, and device identity persist across ordinary reboot. The user now confirms the LED controls work. Firmware contains no HID or LAN listener; Wi-Fi is station-only and control remains on USB UART0.

### M1.1 final user QA (2026-11-16)

User reports the remembered board auto-selects after rescan. Rejecting an unknown-device confirmation leaves the port unopened; UI reports `port was not opened`. User refreshed visible SSIDs successfully and provisioned a hidden SSID. A newly visible network appears, but a disappeared network sometimes remains in the list. User reports LED control works. A source-path review confirms enumeration does not open serial ports, and rejection returns before `Port::open`; no wire-level capture was made. User deferred bridge EEPROM marker provisioning and full UI redesign, so fresh-PC marker recognition and visual redesign are not M1.1 exit gates.

The stale SSID came from reading Windows' available-network cache without requesting a scan. `visible_ssids()` now registers for WLAN scan notifications, calls `WlanScan`, waits up to four seconds for completion, then reads the list. Scan failure or timeout is surfaced instead of presenting cached results as a successful refresh. This uses the existing `windows-sys` dependency. Windows may require location consent for WLAN scanning; manual SSID entry remains available if scanning is denied ([Microsoft WLAN API behavior](https://learn.microsoft.com/en-us/windows/win32/nativewifi/wi-fi-access-location-changes)). User confirms that after Windows stopped seeing a disabled hotspot, refreshing the kvmctl list also removed it. The editable field may retain its current text by design.

SSID discovery uses the host PC's WLAN interfaces through `WlanEnumInterfaces` and `WlanScan`; it does not scan through the ESP32. Saving Wi-Fi settings sends the typed SSID/passphrase to the board for association.

Future LED behavior is deferred beyond M1.1: limit settings to enable/disable and brightness; use red for disconnected, blue for connected/idle, and green while receiving control signals; define extra colors and blink patterns later.

### M1.1 USB descriptor investigation (2026-11-16)

Read-only `pnputil /enum-devices /instanceid ... /properties` on the currently connected CH343 COM6 showed `DEVPKEY_Device_BusReportedDeviceDesc` on the same serial device node, with current value `USB Single Serial`. The node reports VID:PID `1A86:55D3`, WCH manufacturer, location, and loaded WCH VCP driver 2.1.2025.7. The same inventory showed an Espressif COM7; the client candidate filter requires the CH343 VID:PID, so COM7 is excluded by source matching. No administration frame was sent. The current descriptor remains unmodified.

WCH's Windows CH34xSerCfg page lists CH343 support and tool v1.6 (2025-04-09) for manufacturer/product strings. WCH's product page identifies CH343P as having EEPROM-configurable USB descriptors and a USB serial Unique ID. WCH's official configuration README lists CH343P, limits product strings to 18 characters, recommends changing strings rather than VID/PID to distinguish devices, and warns that VID/PID changes break the existing VCP driver match. `kvmctl USB Admin` is 16 characters. Sources: [WCH CH343 product page](https://www.wch.cn/products/CH343.html), [WCH CH34xSerCfg Windows utility](https://www.wch.cn/downloads/CH34xSerCfg_ZIP.html), and [WCH official configuration README](https://github.com/WCHSoftGroup/ch343ser_linux/blob/main/demo/param_config/README.md). Microsoft documents the read-only [bus-reported property](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/devpkey-device-busreporteddevicedesc) as the string provided by the bus driver and retrievable with SetupDiGetDeviceProperty.

Board documentation suggests CH343P, but fitted chip marking or vendor configuration readback has not confirmed the exact variant. User decision on 2026-11-16: defer bridge EEPROM provisioning because misconfiguration could disrupt USB-UART access. No EEPROM write was made. Unknown CH343 devices continue to require explicit confirmation. If provisioning is reconsidered later, identify the fitted variant and save full current configuration before any write; preserve VID/PID, then verify replug/reboot persistence and driver binding.

### M1.1 closure

Closed 2026-11-16 after the user's report that the refreshed list omits a hotspot once Windows no longer sees it. USB board confirmation, hidden-SSID provisioning, and LED controls also passed user QA. No passphrases were shared or logged.

### Wi-Fi refresh button animation

The refresh request now runs on a worker thread while the Win32 message loop remains active. The button cycles through `Refreshing.`, `Refreshing..`, and `Refreshing...` until the host scan completes, then returns to `Refresh list`. This follow-up passed `cargo check --workspace --tests --offline --target x86_64-pc-windows-msvc`; interactive animation has not been run in this session. User can launch it with `cargo run --offline -p kvmctl`.

### M1.4 zero-HID slice started (2026-11-23)

The first M1.4 code slice keeps pinned WSS, signed transcript, and DTLS identity
gates unchanged. `client/src/hid_protocol.rs` encodes and strictly decodes the
protocol 1.0 DataChannel envelope and lease/event payloads without a new
dependency. It rejects non-shortest integers, wrong map sizes, unknown or
out-of-order keys, unsupported types, invalid booleans, unsorted keyboard keys,
invalid button masks, stale epochs, and size limits. The Rust lease model covers
one owner, one pending takeover, explicit rejection, a 10-second window, 1-second
heartbeat, 3-second expiry, release, revoke, and restart cleanup. Shared heartbeat
Shared heartbeat vector is checked by firmware startup through `hid_protocol.c`; the
pure-C lease state machine is also startup-checked through `lease.c`.

Authenticated firmware `on_data()` sends CBOR-looking DataChannel messages through
the strict C decoder and bounded sequence checks. Raw transport-test payloads still
echo for observability; scripted CBOR events receive lease responses and emit zero
HID reports. Physical-input capture remains disabled. Opt-in `signaling-test scripted`
sends key, modifier, button, wheel, and relative-motion events through the secure
path and validates endpoint lease state. It does not enable HID output.

The follow-up response slice adds canonical firmware encoding and bounded per-peer
response queues for `lease_state`, `takeover_notice`, and stable lease errors. The
Rust client now consumes endpoint responses with exact session, direction sequence,
and lease-epoch checks; CBOR input events are no longer echoed by the scripted path.
USB transport status exposes board timestamp, last event sequence, lease epoch,
response queue depth, scripted event/rejection/drop/reorder counts, and normalized
held key/button state.

The host lease vector now includes two enrolled-identity roles and covers explicit
rejection, competing requests, explicit rejection, early release, owner expiry,
10-second takeover timeout, reconnect, revoke, restart, stale epochs, and held-state
cleanup. This is a deterministic model/script check; live two-client hardware
exercise remains open.

Host verification: `cargo test --workspace --offline
--target x86_64-pc-windows-msvc` passed 41 tests; `cargo fmt --all -- --check` and
`git diff --check` passed. Earlier M1.4 baseline changes compiled with the recorded
ESP32-S3 command and strict host `clang` checks passed for `hid_protocol.c` and
`lease.c`; the current telemetry/response edits have not been rerun through those
checks because this host has no `clang`. The pinned product build script remains blocked
because the temporary `esp_peer` source differs from the recorded revision;
reconfiguration of the cached build also needs the unavailable component registry.
The C guard harness still could not run through its Python entry point because that
environment cannot invoke a compatible target compiler.

M1.4 blockers still pending: compile/flash the exact product image on one ESP32-S3;
run the live two-client/two-enrolled-identity takeover flow;
verify HID reports with Windows Raw Input filtered to the selected board identity;
and record build size, CPU, internal heap, PSRAM, setup/reconnect, queue/drop/reorder,
transport RTT, board-local, and send-to-HID timings. PC Ethernet and pinned known-IP
remain acceptance setup. Cross-user DPAPI and Surfshark-on DNS-SD stay deferred to M2.

### WSL firmware fix: Board 2 Windows retest (2026-11-23)

The firmware fix makes USB transport sessions exclusive and treats disconnects
after both DataChannels open as clean teardown. The WSL host signaling guard
harness passed, covering transport admission, disconnect status, two-peer
isolation, signaling deadlines, and revoke persistence retry.

The user ran the Windows CLI from clean checkout `a1b8ab5` in native PowerShell
against Board 2 on `COM6` (ESP32-S3 N16R8 rev v0.2, GPIO48 RGB). The uncommitted
WSL build at `firmware/build-wsl/kvmctl_firmware.bin` was 1,208,704 bytes.
The Windows copy matched SHA-256
`1c3017532475ceaf6c3f12f87ff27a3b4d5ae4c86bee8e3f6a301dcf2faee72c`.

Flash verified the 2,031,616-byte factory app backup before writing, verified
the replacement digest, reset the board, and exited 0. The Windows backup is
`firmware/backups/factory-app-before-bench-fix-COM6-20261123-230002.bin`.
The previous verified backup was preserved. The Windows image copy is in the
Git-ignored `firmware/build/kvmctl_firmware-bench-fix.bin`.

All four post-flash commands exited 0.

| Command | Result |
| --- | --- |
| `status COM6` | Firmware `a1b8ab5-dirty`. Wi-Fi connected. |
| `transport-test COM6` | Initial `conflicting_sdp=rejected` and reconnect `malformed_sdp=rejected`. Both stages reported `device_state=0` and `active_session=closed`. Finished with `M1.2 TRANSPORT COMPLETE sessions=2 reconnect=clean`. |
| `signaling-test COM6` | Authorized, echoed 10 messages each on control and motion, reported `state=0` on `M1.3 RESOURCE`, and reached `M1.3 SIGNALING COMPLETE`. |
| `signaling-test COM6 scripted` | Same authorization, echo, resource state, and completion checks passed. Also printed `M1.4 SCRIPTED` with `hid_reports=0`. |

This run completes the single-board build and flash retest for this firmware
fix. Physical USB HID output and live two-client takeover remain open for M1.4.

### M1.4 live takeover on Board 2 (2026-11-24)

Windows `main` was clean at `5d123b1ad7fcb9b75c790922a884a065997412c9`
before changes. `git cat-file -t
5d123b1` returned `commit` and `git merge-base --is-ancestor 5d123b1 HEAD`
exited 0. The client used the working tree based on that revision. The initial
COM6 status reported firmware `a1b8ab5-dirty` and Wi-Fi connected. The local
copy of the previously verified Board 2 app image matched SHA-256
`1c3017532475ceaf6c3f12f87ff27a3b4d5ae4c86bee8e3f6a301dcf2faee72c`.

```powershell
cargo run --offline -p kvmctl -- status COM6
cargo run --offline -p kvmctl -- enroll COM6
cargo run --offline -p kvmctl -- enroll COM6 takeover
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
```

Sandboxed enrollment reported `Windows DPAPI operation failed`. Native-profile
enrollment confirmed the existing A credential and enrolled a new B credential.
Both are DPAPI-protected under the same Windows user, with fingerprints
`64df35de642ae1a5e123912b3ba3101abfead24e655ae8efc5ca1826e1d7edd5` (A) and
`70aa21bad6f16f9a6fb43ee544cdace504bb790097915454ce4b67953080580e` (B).
On the prior image A opened both DataChannels and completed transcript authorization.
B's signaling setup returned `identity_failed` before its DataChannels opened,
so the original image produced no live lease ordering evidence.

The diagnostic app was built in Ubuntu WSL from `5d123b1` plus the
working-tree transport diagnostics, using ESP-IDF 6.1.1 and esp-webrtc-solution
`3971094e2e7c2ab7cb4de59e4eae710e1902d2b4`.

```bash
source ~/esp/esp-idf-v6.1.1/export.sh
export KVMCTL_LIBSRTP_SOURCE="$HOME/esp/espressif__esp_libsrtp"
bash firmware/build-transport.sh "$HOME/esp/esp-webrtc-v1.3.0" build
cp firmware/build-wsl/kvmctl_firmware.bin "$windows/firmware/build/kvmctl_firmware-m14-diagnostic.bin"
sha256sum "$windows/firmware/build/kvmctl_firmware-m14-diagnostic.bin"
```

The build used `windows=/mnt/c/Users/night/workspace/kvmctl` and temporarily
copied the Windows working-tree `firmware/main/transport.c` into the WSL checkout.
The copied Windows app is 1,209,104 bytes, SHA-256
`112be2cc267f26137f74492999c023cdba4e2ea4e2a2ab13d4c2b4f20c1aa0aa`.
The WSL source file was restored after the build.

```powershell
certutil -hashfile .\firmware\build\kvmctl_firmware-m14-diagnostic.bin SHA256
'yes' | cargo run --offline -p kvmctl -- flash COM6 .\firmware\build\kvmctl_firmware-m14-diagnostic.bin .\firmware\backups\factory-app-before-m14-diagnostic-COM6-20261124.bin
certutil -hashfile .\firmware\backups\factory-app-before-m14-diagnostic-COM6-20261124.bin SHA256
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
```

The CLI verified the 2,031,616-byte factory app backup at `0x10000` before
writing. Backup SHA-256 is
`24def7c13f819c5bda10455cbeb1d98c14323cb05900bf71b6689d4b6e397ff1`.
Its first 1,208,704 bytes match the prior bench image SHA-256 above.
The CLI verified the replacement digest and reset the board. Two diagnostic
takeover runs again authorized A and failed during B setup. The second run
reported `DTLS peer open failed (-2), internal free=20079 largest=7680`.
The internal heap minima were 3,795 and 4,591 bytes. Both runs ended at epoch
0 with held keys and buttons clear. USB transport status reported state 4
(failed). No lease handoff was exercised.

The follow-up zero-HID build moves the two fixed-size SDP offer/answer pairs
from internal RAM to PSRAM, matching the existing WSS SDP allocation. The
buffers remain bounded and are cleared on cleanup. TLS allocation settings
and pinned signaling checks stay in place.

The same WSL build command produced the 1,209,088-byte PSRAM app, SHA-256
`0ef17f528e97fc14b838b29c17ea3679fea63c30e899b18208d332b71db69936`.

```powershell
certutil -hashfile .\firmware\build\kvmctl_firmware-m14-peer-psram.bin SHA256
'yes' | cargo run --offline -p kvmctl -- flash COM6 .\firmware\build\kvmctl_firmware-m14-peer-psram.bin .\firmware\backups\factory-app-before-m14-peer-psram-COM6-20261124.bin
certutil -hashfile .\firmware\backups\factory-app-before-m14-peer-psram-COM6-20261124.bin SHA256
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
```

Passing host checks included `cargo test --workspace --offline` (41 tests),
`cargo fmt --all -- --check`, `git diff --check`, and the WSL runs
`python3 firmware/test_signaling_guards.py` and `python3 firmware/test_usb_admin.py`
against the Windows working tree.

The app-only swap verified its backup and replacement, then reset Board 2.
Backup SHA-256 was
`42bb60cb081d71fbf08138107e5f754aeb3d2ae4c5b0ffea0167a43f56499a50`.
Its first 1,209,104 bytes match the diagnostic app hash. The first takeover
attempt immediately after reset timed out reading a complete USB status
response. A subsequent `cargo run --offline -p kvmctl -- status COM6`
succeeded and reported `5d123b1-dirty` with Wi-Fi connected.

On the PSRAM SDP image, A authorized and B passed offer setup, then timed out
waiting 25 seconds for a DataChannel. Internal free heap fell to 23 bytes.
The run ended at USB transport state 0, epoch 0, and empty held state.
No lease handoff occurred. The follow-up configuration selects ESP-IDF's
PSRAM allocator for mbedTLS to relieve internal heap pressure. This puts
mbedTLS dynamic allocations in external RAM. Physical board compromise is
outside the current threat model. See [design.md](design.md).

The SDP-only image passed `cargo run --offline -p kvmctl -- transport-test COM6`
with two clean sessions and both SDP rejection checks. It also passed
`cargo run --offline -p kvmctl -- signaling-test COM6 scripted 192.168.50.137`.
Scripted telemetry ended at epoch 2, USB transport state 0, response queue
depth 0, empty held state, and zero rejections, drops, and reorders.
The firmware has no USB HID emitter. The client reported `hid_reports=0`
and `physical_input_capture=off`.

The mbedTLS PSRAM app was built from the same WSL revision with the Windows
transport and allocation changes. The build set `CONFIG_MBEDTLS_EXTERNAL_MEM_ALLOC=y`
in the existing generated sdkconfig before the WSL command below and verified
the effective setting. It restored the WSL sources and cached sdkconfig afterward.
The Windows image is 1,209,088 bytes, SHA-256
`7fdf67c70cbb581d3df4e2b6ba7c170c13602a0ad88a4d9b63fb5d758a73216d`.

```bash
bash firmware/build-transport.sh "$HOME/esp/esp-webrtc-v1.3.0" reconfigure build
sha256sum "$windows/firmware/build/kvmctl_firmware-m14-tls-psram.bin"
```

```powershell
certutil -hashfile .\firmware\build\kvmctl_firmware-m14-tls-psram.bin SHA256
'yes' | cargo run --offline -p kvmctl -- flash COM6 .\firmware\build\kvmctl_firmware-m14-tls-psram.bin .\firmware\backups\factory-app-before-m14-tls-psram-COM6-20261124.bin
certutil -hashfile .\firmware\backups\factory-app-before-m14-tls-psram-COM6-20261124.bin SHA256
Start-Sleep -Seconds 8
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
```

The CLI verified the 2,031,616-byte backup and replacement digest, then reset
Board 2. Backup SHA-256 was
`6e21f103f20a171e5a25d07fad1ccd70075b9b831c965b92bafc11a6fb77e41c`.
Both Windows clients completed pinned WSS, DTLS and transcript authorization
while connected concurrently. DataChannels opened in 3,983 ms (A) and 3,889 ms (B).
The takeover command exited 0.

| Step | Observed endpoint response or telemetry |
| --- | --- |
| A acquires | A response sequence 0, lease state `owned` by A, epoch 1. Held keys/buttons became 1/1. |
| B requests, A rejects | Request `846ef0c95374fb16909bb629e632a939`. B sequence 0 `pending`, A sequence 1 notice with 10,000 ms, B sequence 1 `accepted=false`. Both sequence 2 responses returned A as owner at epoch 1. |
| B requests again | Request `2cd6ef6f3dd23090a76a6f5bda27cf2a`. B sequence 3 `pending`, A sequence 3 notice with 10,000 ms. Board timestamp 21,357 ms. |
| A stays active | Nine heartbeats preserved epoch 1 and held state 1/1. Last heartbeat sequence 13 at board timestamp 30,613 ms. At heartbeat 8, B sequence 4 rejected B's input with `stale_epoch` at epoch 1. |
| Timeout handoff | Both peers reported B as sole owner at epoch 3 (A sequence 4, B sequence 5). Client elapsed time 10,141 ms. USB telemetry showed held state 0/0. |
| Input ordering | A sent a key using epoch 1. A sequence 5 returned `stale_epoch` at endpoint epoch 3. USB held state stayed 0/0. Only then did B send epoch-3 key/button input, which set held state 1/1. |
| Release and close | Both sequence 6 responses reported no owner at epoch 4. Final USB state 0, queue depth 0, held state 0/0, 21 events, two expected stale input drops, zero parser rejections and reorders. |

The client reported `hid_reports=0` and `physical_input_capture=off`.
The takeover image linked no USB HID emitter. Internal heap start/minimum/after
was 204,947/84,371/149,987 bytes, and PSRAM was 8,292,188/7,374,028/8,260,336
bytes. These are two-peer bench readings for this run.

Intermediate repeats using the same command exposed timing sensitivity in the
client. The run at epoch 5 failed its echo check after heartbeat 8 and closed
at epoch 8. The run at epoch 9 exceeded a per-heartbeat observation deadline
and closed at epoch 10. The run at epoch 11 could no longer verify A's held
state at its final sample and closed at epoch 14. All three ended with USB
state 0, empty held state, and zero HID. The updated scenario sends heartbeats
on a 750 ms asynchronous schedule and checks the most recent accepted
sequence through USB telemetry before waiting for the grant.

### M1.4 scripted USB HID on Board 2 (2026-11-24)

Board 2's native USB cable was already connected. Before switching to the
bench image, `pnputil /enum-devices /instanceid 'USB\VID_303A&PID_1001&MI_00\6&2a6611de&0&0000' /relations`
showed COM7's parent serial `7C:4F:AD:BC:C5:4C`, matching the COM6 flash
probe's board MAC. The bench changes the native device to a keyboard and
mouse with VID:PID `303A:4004` and fingerprint-derived serial
`e299faff8cb8d99166647efa`. The CH343 administration port is COM6.

The opt-in app was built under Ubuntu WSL from `5d123b1` and the Windows
working-tree firmware changes. The build reused the pinned M0 caches for
`esp_tinyusb` 2.4.0 and `tinyusb` 0.21.0~6. SDK configuration confirmed
`CONFIG_MBEDTLS_EXTERNAL_MEM_ALLOC=y` and `CONFIG_TINYUSB_HID_COUNT=1`.
The image is 1,240,608 bytes with SHA-256
`346b273a211f99b0e55af204211891cd603b5d0906d61cd8ca43e875cb0d4f4d`.
WSL inputs were restored after the build.

```bash
source ~/esp/esp-idf-v6.1.1/export.sh
export KVMCTL_LIBSRTP_SOURCE="$HOME/esp/espressif__esp_libsrtp"
export KVMCTL_HID_BENCH_COMPONENTS="$windows/verification/m0/usb-hid-uart/managed_components"
KVMCTL_HID_BENCH=1 bash firmware/build-transport.sh "$HOME/esp/esp-webrtc-v1.3.0" reconfigure build
cp firmware/build-wsl/hid-bench/kvmctl_firmware.bin "$windows/firmware/build/kvmctl_firmware-m14-hid-bench.bin"
sha256sum "$windows/firmware/build/kvmctl_firmware-m14-hid-bench.bin"
```

```powershell
certutil -hashfile .\firmware\build\kvmctl_firmware-m14-hid-bench.bin SHA256
'yes' | cargo run --offline -p kvmctl -- flash COM6 .\firmware\build\kvmctl_firmware-m14-hid-bench.bin .\firmware\backups\factory-app-before-m14-hid-bench-COM6-20261124.bin
certutil -hashfile .\firmware\backups\factory-app-before-m14-hid-bench-COM6-20261124.bin SHA256
Start-Sleep -Seconds 8
cargo run --offline -p kvmctl -- status COM6
```

The CLI verified the 2,031,616-byte backup before writing, verified the
replacement digest, and reset Board 2. Backup SHA-256 was
`5d16b550e6069a6c02874aff11e0ba666ada67fbda1368a211f76c1b1527bebc`.
Its app bytes match the preceding zero-HID mbedTLS PSRAM image hash.
COM6 status returned firmware `5d123b1-dirty` and Wi-Fi connected.

```powershell
pnputil /enum-devices /instanceid 'USB\VID_303A&PID_4004\e299faff8cb8d99166647efa' /relations
cargo run --offline -p kvmctl -- signaling-test COM6 hid-bench 192.168.50.137
```

Windows enumerated `HID\VID_303A&PID_4004&Col01\6&33db1976&0&0000` and
`HID\VID_303A&PID_4004&Col02\6&33db1976&0&0001` as children of the selected
native USB device. The Raw Input observer matched both collections to that
exact VID/PID/serial parent. Both pinned DataChannels opened in 4,201 ms,
and the endpoint and client transcript proofs passed. The command exited 0.

```text
M1.4 RAW INPUT shift=make-break key_a=make-break left_button=down-up vertical=120 horizontal=-120 relative=(12,-4) release=true
M1.3 SIGNALING COMPLETE scenario=hid-bench hid_reports=7 physical_input_capture=off
```

The seven completed USB reports were keyboard press, mouse button press,
vertical wheel, horizontal wheel, relative motion, keyboard release, and
mouse release. The script issued a lease release to trigger both neutral
reports. Final transport state 0, lease epoch 2, queue depth 0, key/button
state 0/0, eight scripted events, and no rejections, drops, or reorders.
Internal free heap start/minimum/after was 194,827/130,235/170,023 bytes.
PSRAM was 8,292,172/7,852,940/8,289,256 bytes. Control RTT min/max was
13,850/114,627 µs. Motion RTT min/max was 16,229/28,822 µs.

The default WSL build from the same tree also passed with the bench flag
unset and no `KVMCTL_HID_BENCH` define in its compile commands. The default image
has SHA-256 `b9f1a73058d1de69e08254a7bb4092a2207b09d8f78bcce8a46076b5e07cc90c`.
The WSL signaling and USB decoder checks passed again, along with all 41
Windows Rust tests. The default image was built for verification only.

Post-bench transport checks ran with the HID-capable app still installed
and the lifetime report count already at seven. Each command required
the count to remain unchanged.

```powershell
cargo run --offline -p kvmctl -- transport-test COM6
cargo run --offline -p kvmctl -- signaling-test COM6 scripted 192.168.50.137
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
cargo run --offline -p kvmctl -- signaling-test COM6 takeover 192.168.50.137
```

All four exited 0 with `hid_reports=0` for the run. USB transport returned
to idle after each of two sessions, rejected conflicting and malformed SDP,
and reconnected cleanly. Single-client scripted signaling ended idle at
epoch 4 with no drops or reorders.

The final takeover scenario passed twice, with A/B/idle epochs 5/7/8 and
9/11/12. Both runs saw the explicit rejection and A's continuing ownership,
then confirmed the latest heartbeat at control sequence 15 and A's held
state before handoff. B's early input returned `stale_epoch` at A's epoch.
The no-response handoffs arrived after 10,323 ms and 10,237 ms. In each, both
clients reported B as sole owner, USB held state cleared, A's old-epoch input
returned `stale_epoch` at B's epoch, and B's subsequent key/button set held
state. Both peers received idle at the final epoch. Each final USB status
showed transport state 0, queue depth 0, held state 0/0, 23 processed events,
two intentional stale-input drops, and no parser rejections or reorders.
Internal heap minima were 69,459 and 69,335 bytes. PSRAM minima were
7,372,592 and 7,373,156 bytes.

Physical release was observed on explicit lease release. Physical release
on disconnect, heartbeat expiry, takeover, revoke, restart, and USB
transfer failure was not separately exercised. Sustained load, total SoC
CPU, and send-to-HID timing remain unmeasured. Firmware images and verified
factory-app backups are in ignored build and backup directories.
