"""Static guards for M1.5 reset timing, erase scope, and USB recovery wiring."""

from pathlib import Path


ROOT = Path(__file__).parent
recovery = (ROOT / "main" / "recovery.c").read_text()
app = (ROOT / "main" / "app_main.c").read_text()
client = (ROOT.parent / "client" / "src" / "main.rs").read_text()
writers = {
    "client_auth.c": (ROOT / "main" / "client_auth.c").read_text(),
    "wifi_setup.c": (ROOT / "main" / "wifi_setup.c").read_text(),
    "led_control.c": (ROOT / "main" / "led_control.c").read_text(),
}

assert "#define BOOT_GPIO GPIO_NUM_0" in recovery
assert "#define BOOT_HOLD_US (10LL * 1000LL * 1000LL)" in recovery
assert "pressed_since = 0" in recovery
assert "nvs_erase_all" in recovery
for namespace in ("kvmctl_wifi", "kvmctl_id", "kvmctl_auth", "kvmctl_cfg"):
    assert f'"{namespace}"' in recovery
assert "esp_restart()" in recovery
assert "recovery_start();" in app
assert app.index("recovery_start();") < app.index("usb_admin_start();")
for name, source in writers.items():
    assert "recovery_storage_write_lock();" in source, name
    assert "recovery_storage_write_unlock();" in source, name
assert '"recover"' in client
assert "ClientCredential::generate()" in client
assert "install_replacement(false)" in client
print("M1.5 recovery timing, erase scope, and USB recovery guards passed")
