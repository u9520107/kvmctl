# M1.1 firmware

First product firmware increment clears the onboard addressable RGB LED at the
start of `app_main`; it has no startup animation and defaults to off.

Build with ESP-IDF 6.1.1:

```powershell
cd firmware
idf.py build
```

Select the LED wiring under **kvmctl hardware profile** in `idf.py menuconfig`.
GPIO48 is the default. Espressif documents GPIO48 for ESP32-S3-DevKitC-1 v1.0
and GPIO38 for v1.1, so confirm the board revision/profile before flashing.
[Espressif's FAQ](https://docs.espressif.com/projects/esp-faq/en/latest/hardware-related/development-board.html#which-gpio-is-connected-to-the-rgb-led-of-the-esp32-s3-devkitc-1-development-board)
records this revision difference.
Boards without a supported onboard RGB LED can select **No supported onboard
RGB LED**; firmware logs that LED controls are unavailable.
