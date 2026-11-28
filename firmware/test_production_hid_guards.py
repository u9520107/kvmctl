"""Static guards for production Mini HID output wiring."""

from pathlib import Path


ROOT = Path(__file__).parent
transport = (ROOT / "main" / "transport.c").read_text()
project = (ROOT / "CMakeLists.txt").read_text()
main_cmake = (ROOT / "main" / "CMakeLists.txt").read_text()
hid = (ROOT / "hid_bench" / "hid_bench.c").read_text()

assert "#ifdef KVMCTL_HID_BENCH" not in transport
assert "#if defined(KVMCTL_HID_BENCH)" not in transport
assert 'ENV{KVMCTL_HID_BENCH}' not in project
assert 'ENV{KVMCTL_HID_BENCH}' not in main_cmake
assert '"${CMAKE_CURRENT_LIST_DIR}/hid_bench"' in project
assert "hid_bench led_strip" in main_cmake
assert "hid_bench_enqueue(event)" in transport
assert "hid_bench_init(fingerprint)" in transport
assert "metrics->hid_reports = hid_bench_reports()" in transport
assert "lease_heartbeat(&s_lease" in transport
assert "report.modifiers = event->modifier_mask" in hid
assert "tud_hid_keyboard_report" in hid
assert "tud_hid_mouse_report" in hid
assert "hid_bench_disarm" in transport

print("production Mini HID wiring guards passed")
