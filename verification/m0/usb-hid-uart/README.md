# M0 USB HID and UART probe

Temporary M0 feasibility firmware for ESP32-S3. Enumerates keyboard and mouse over native USB without reading physical input or sending HID reports. CH343 UART0 accepts `PING`, `STATUS`, and a Wi-Fi setup command from the local Windows form. Wi-Fi starts only after the command arrives; credentials use RAM storage and are not saved to flash. The board reports connection status and advertises `kvmctl-m0-probe.local` through mDNS. No service listener or control protocol is exposed.

Build with ESP-IDF 6.1.1 for esp32s3:

    idf.py set-target esp32s3
    idf.py build

Launch the visible Windows setup form from PowerShell:

    powershell.exe -NoProfile -ExecutionPolicy Bypass -File verification/m0/wifi-setup.ps1

Select the CH343 COM port, enter the 2.4 GHz Wi-Fi name and password locally, then select **Connect board**. The form clears both fields after sending credentials over USB serial. Status shows Wi-Fi state, assigned IP, and mDNS initialization. The M0 probe accepts one connection attempt per boot; reset the board before retrying.

If flashing a board, first preserve its original app and partition evidence. Use the CH343 COM port (`COM3` on board 1, `COM6` on board 2 in this inventory). Flash only the app at 0x10000 with esptool; do not run idf.py flash because that can write bootloader and partition data:

    python -m esptool --chip esp32s3 --port COM6 --baud 115200 --before default-reset --after hard-reset write-flash --flash-mode dio --flash-size 16MB --flash-freq 80m 0x10000 build/m0_usb_hid_uart.bin

This is M0 test firmware, not product firmware. It does not initialize NVS or persist Wi-Fi credentials. Do not pass credentials through command-line arguments or chat.
