# Third-party notices

These notices apply to source, firmware images, and Windows archives produced
from this repository. Keep this file with every redistributed archive or image
package. The checked-in component notices under
`firmware/third_party/esp_peer/LICENSES/` remain authoritative for that
component.

## JetKVM Mini

The source-built peer is imported from JetKVM Mini:

- Upstream: https://github.com/jetkvm/mini
- License: https://github.com/jetkvm/mini/blob/9683035eb3b5169c176a7025b98d317e2f6058ac/LICENSE
- Path: `third_party/esp_peer`
- Matching upstream base: `9683035eb3b5169c176a7025b98d317e2f6058ac`
- Local import: `7b9ac6b`

JetKVM Mini is MIT-licensed. The upstream notice is `Copyright (c) 2026
JetKVM`; the upstream copyright and attribution notices remain with imported
files.

```text
MIT License

Copyright (c) 2026 JetKVM

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## libpeer

The peer contains libpeer-derived code by sepfy. See
`firmware/third_party/esp_peer/LICENSES/libpeer-MIT.txt`.

```text
MIT License

Copyright (c) 2021 sepfy

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Espressif Modified MIT

The Espressif peer wrapper and retained Espressif portions use the following
license. See `firmware/third_party/esp_peer/LICENSE` and its license copy.

```text
Espressif Modified MIT License

Copyright (c) 2025 Espressif Systems (Shanghai) CO., LTD

Permission is hereby granted for use EXCLUSIVELY with Espressif Systems products.
This includes the right to use, copy, modify, merge, publish, distribute and sublicense
the Software, subject to the following conditions:

1. This Software MUST BE USED IN CONJUNCTION WITH ESPRESSIF SYSTEMS PRODUCTS.
2. The above copyright notice and this permission notice shall be included in all copies
or substantial portions of the Software.
3. Redistribution of the Software in source or binary form FOR USE WITH NON-ESPRESSIF PRODUCTS
is strictly prohibited.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE
FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

## Locked firmware component inventory

The ESP-IDF 6.1.1 ESP32-S3 build links these pinned release components from
`firmware/main/idf_component.yml`, `firmware/hid_bench/idf_component.yml`, and
`firmware/dependencies.lock`: `esp_peer` 1.5.5 source override,
`esp_libsrtp` 1.0.0, `esp_tinyusb` 2.4.0, `tinyusb` 0.21.0~6,
`led_strip` 3.0.3, `mdns` 1.14.1, and ESP-IDF 6.1.1. The release component
license files must remain beside any cached component source used for a build;
the Windows build runner copies this notice, the root license, the checked-in
peer licenses, and discovered component license files into `notices/` beside
each produced firmware image.

The locked Windows release graph was audited with:

```powershell
cargo tree --workspace --locked --target x86_64-pc-windows-msvc --edges normal,build
```

`Cargo.lock` contains 215 package stanzas. Direct normal/build dependencies
are `anyhow`, `async-trait`, `base64`, `bytes`, `log`, `rtc`, `rustls`, `sha1`,
`webrtc`, `windows-sys`, and `x509-parser`; transitive licenses remain those
declared by each crate in the locked graph. The firmware lock still records a
registry `esp_peer` 1.5.3 entry for historical component resolution, while
production CMake overrides it with the checked-in 1.5.5 source. A clean S3
build must continue to verify the actual linked source and release components.
