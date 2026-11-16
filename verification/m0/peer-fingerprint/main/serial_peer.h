#pragma once

/* Blocking UART bench, called after a usable IP interface is up.
 * Owns stdin until reboot; no credentials, Wi-Fi setup, HID or NVS here.
 * Run from a task with at least 24 KiB stack (certificate generation).
 */
void run_serial_peer(void);
