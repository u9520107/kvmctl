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
assert "static int accept_mini_event" in transport
assert "result = accept_scripted_event_locked" in transport
mini_accept = transport[transport.index("static int accept_mini_event"):transport.index("/* Mini HIDRPC", transport.index("static int accept_mini_event"))]
assert mini_accept.count("lease_lock()") == 1
assert "accept_scripted_event(" not in mini_accept
mini_path = transport[transport.index("if ((input_channel || motion_channel)"):transport.index("if (frame->size > 0 && (frame->data[0] >> 5) == 5)")]
assert "accept_mini_event" in mini_path
assert "mini_lease_ready" not in mini_path
assert "lease_set_held" not in mini_path
assert "note_mini_motion" in mini_path
assert "mini_motion_interval_min_us" in transport
assert "mini_lease_lock_wait_max_us" in transport
assert "hid_bench_fifo_high_water" in transport
assert "hid_bench_fifo_overflows" in transport
assert "hid_bench_reset_fifo_metrics()" in transport

print("production Mini HID wiring guards passed")
