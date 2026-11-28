"""Run production lease, HID codec and transport arm rules with fake USB service."""

import argparse
import ctypes
from pathlib import Path
import subprocess
import sys
import tempfile


firmware = Path(__file__).resolve().parent
main = firmware / "main"
transport = (main / "transport.c").read_text()


def body(start, end):
    return transport[transport.index(start):transport.index(end, transport.index(start) + 1)]


harness = r"""
#include "hid_protocol.h"
#include "lease.h"
#include <string.h>
#define KVMCTL_HID_BENCH 1
#define MAX_TRANSPORT_PEERS 2
#define atomic_load(p) (*(p))
#define atomic_store(p, value) (*(p) = (value))
#define atomic_fetch_add(p, value) (*(p) += (value))
#define lease_lock() ((void)0)
#define lease_unlock() ((void)0)
#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

typedef struct {
    bool active, failed, usb, close_requested, application_authorized, session_id_seen;
    bool heartbeat_ack_supported, live_input_protocol;
    uint32_t generation;
    uint8_t session_id[16];
    uint8_t client_fingerprint[32];
} transport_peer_t;
typedef struct { int unused; } lease_log_snapshot_t;
static transport_peer_t s_peers[MAX_TRANSPORT_PEERS];
static transport_peer_t *s_bench_owner;
static lease_state_t s_lease;
static bool s_lease_cleanup_pending, s_live_report_pending;
static uint64_t s_live_report_sequence;
static struct {
    transport_peer_t *peer;
    uint32_t epoch, generation;
    uint64_t sequence;
} s_live_disarm;
static uint64_t s_last_event_timestamp_ms, s_last_event_sequence, s_scripted_drops;
static bool usb_attached = true, usb_held, usb_cleanup;
static bool usb_report_pending;
static bool drop_next_motion;
static kvmctl_hid_event_t usb_queued_report;
static uint8_t usb_sent_buttons;
static int usb_drag_x, usb_drag_y;
static kvmctl_hid_event_t response;
static uint8_t fixture[3 * (KVMCTL_HID_MAX_CONTROL + 2)];
static size_t fixture_size;

static bool append_fixture(void)
{
    size_t length = kvmctl_hid_encode(&response, fixture + fixture_size + 2,
                                      sizeof(fixture) - fixture_size - 2);
    if (!length) return false;
    fixture[fixture_size] = (uint8_t)length;
    fixture[fixture_size + 1] = (uint8_t)(length >> 8);
    fixture_size += length + 2;
    return true;
}

#ifdef _WIN32
__declspec(dllexport)
#endif
const uint8_t *fixture_data(size_t *length)
{
    *length = fixture_size;
    return fixture;
}

static const uint8_t *lease_slot_identity(int slot) { return s_peers[slot].client_fingerprint; }
static bool hid_bench_can_arm(void) { return usb_attached && !usb_cleanup && !usb_report_pending; }
static void hid_bench_disarm(void)
{
    usb_cleanup = usb_held;
    usb_report_pending = false;
}
static bool hid_bench_service(bool *detached)
{
    *detached = !usb_attached;
    if (*detached) {
        usb_cleanup = usb_report_pending = usb_held = false;
        usb_sent_buttons = 0;
    }
    return true;
}
static bool hid_bench_cleanup_complete(void) { return !usb_cleanup && !usb_report_pending; }
static bool hid_bench_releasing(void) { return false; }
static const char *hid_bench_enqueue(const kvmctl_hid_event_t *event)
{
    if (!usb_attached) return "invalid_capability";
    if (event->type == KVMCTL_HID_MOTION && drop_next_motion) {
        drop_next_motion = false;
        return "resource_exhausted";
    }
    if (event->type == KVMCTL_HID_KEYBOARD_STATE) usb_held = !!event->key_count;
    if (event->type == KVMCTL_HID_MOUSE_BUTTONS) usb_held = !!event->button_mask;
    usb_queued_report = *event;
    usb_report_pending = true;
    return NULL;
}
static void complete_usb_report(void)
{
    if (!usb_report_pending) return;
    if (usb_queued_report.type == KVMCTL_HID_MOUSE_BUTTONS)
        usb_sent_buttons = usb_queued_report.button_mask;
    if (usb_queued_report.type == KVMCTL_HID_MOTION && usb_sent_buttons) {
        usb_drag_x += usb_queued_report.first_delta;
        usb_drag_y += usb_queued_report.second_delta;
    }
    usb_report_pending = false;
}
static int64_t esp_timer_get_time(void) { return 1000; }
static void lease_tick_locked(uint64_t now, lease_log_snapshot_t *log) { (void)now; (void)log; }
static void identity_prefix(const uint8_t *identity, char *text) { (void)identity; *text = 0; }
#define lease_log_locked(...) ((void)0)
#define emit_lease_log(...) ((void)0)
#define queue_notice_locked(...) ((void)0)
#define queue_takeover_response_locked(...) ((void)0)
static bool queue_response(transport_peer_t *slot, const kvmctl_hid_event_t *event)
{
    uint8_t encoded[KVMCTL_HID_MAX_CONTROL];
    size_t length = kvmctl_hid_encode(event, encoded, sizeof(encoded));
    return slot->application_authorized && kvmctl_hid_decode(encoded, length, false, &response);
}

SESSION_AND_DISARM
STATE_REPLY
ERROR_REPLY
CURRENT_EVENT_AND_DISPATCH
static void fail_session(transport_peer_t *slot) { slot->failed = true; }
static void finish_lease_cleanup_locked(lease_log_snapshot_t *log)
{
    (void)log;
    if (s_lease_cleanup_pending && hid_bench_cleanup_complete()) {
        s_lease_cleanup_pending = false;
        queue_state_to_peers_locked();
    }
}
HID_WORKER

static int check_drag(int owner, kvmctl_hid_event_t *event, unsigned int mouse_hz)
{
    lease_log_snapshot_t log = {0};
    const int samples = mouse_hz * 16 / 1000;
    int initial_x = usb_drag_x;
    int initial_y = usb_drag_y;

    event->type = KVMCTL_HID_MOUSE_BUTTONS;
    event->button_mask = 1;
    ++event->sequence;
    CHECK(accept_scripted_event(owner, event, false) == 1);
    complete_usb_report();
    service_bench_locked(1, &log);
    CHECK(usb_sent_buttons == 1 && response.request_sequence == event->sequence);
    for (int slot = 0; slot < 10; ++slot) {
        event->type = KVMCTL_HID_MOTION;
        event->first_delta = samples;
        event->second_delta = -2 * samples;
        uint64_t report_sequence = ++event->sequence;
        CHECK(accept_scripted_event(owner, event, true) == 1);
        CHECK(!s_live_report_pending && usb_sent_buttons == 1);
        complete_usb_report();
        service_bench_locked(1, &log);
        CHECK(response.request_sequence != report_sequence);
        event->type = KVMCTL_HID_HEARTBEAT;
        ++event->sequence;
        CHECK(accept_scripted_event(owner, event, false) == 2);
        service_bench_locked(1, &log);
        CHECK(!s_live_report_pending && response.request_sequence != report_sequence);
    }
    event->type = KVMCTL_HID_MOUSE_BUTTONS;
    event->button_mask = 0;
    ++event->sequence;
    CHECK(accept_scripted_event(owner, event, false) == 1);
    CHECK(usb_sent_buttons == 1);
    CHECK(usb_drag_x - initial_x == 10 * samples && usb_drag_y - initial_y == -20 * samples);
    complete_usb_report();
    service_bench_locked(1, &log);
    CHECK(!usb_sent_buttons && !s_lease.held_buttons && !s_live_report_pending);
    CHECK(response.request_sequence == event->sequence);
    return 0;
}

#ifdef _WIN32
__declspec(dllexport)
#endif
int run_checks(void)
{
    const int owner = 0;
    transport_peer_t *slot = &s_peers[owner];
    kvmctl_hid_event_t event = { .session_id = {1} };
    lease_log_snapshot_t log = {0};
    CHECK(lease_check_vectors() && kvmctl_hid_check_vectors());
    slot->active = true;
    slot->application_authorized = true;
    slot->client_fingerprint[0] = 1;
    lease_init(&s_lease);
    event.type = KVMCTL_HID_LEASE_REQUEST;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    event.lease_epoch = s_lease.epoch;
    uint32_t epoch = event.lease_epoch;
    CHECK(!response.request_sequence_present);
    event.type = KVMCTL_HID_HEARTBEAT;
    event.sequence = 1;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    CHECK(!response.request_sequence_present);

    event.type = KVMCTL_HID_LIVE_INPUT_ARM;
    event.sequence = 2;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    CHECK(s_bench_owner == slot && response.type == KVMCTL_HID_LEASE_STATE);
    CHECK(response.request_sequence_present && response.request_sequence == 2);
    CHECK(append_fixture());
    event.type = KVMCTL_HID_KEYBOARD_STATE;
    event.sequence = 3;
    event.key_count = 1;
    CHECK(accept_scripted_event(owner, &event, false) == 1);
    CHECK(usb_held && s_lease.held_keys && s_live_report_pending);
    CHECK(response.request_sequence == 2);
    service_bench_locked(1, &log);
    CHECK(s_live_report_pending && response.request_sequence == 2);
    complete_usb_report();
    service_bench_locked(1, &log);
    CHECK(!s_live_report_pending && response.request_sequence == 3);

    event.type = KVMCTL_HID_LIVE_INPUT_DISARM;
    event.sequence = 4;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    CHECK(!s_bench_owner && !s_live_report_pending && usb_cleanup);
    CHECK(lease_held_empty(&s_lease) && s_lease.active && s_lease.epoch == epoch);
    CHECK(response.request_sequence == 3);

    event.type = KVMCTL_HID_LIVE_INPUT_ARM;
    event.sequence = 5;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    CHECK(!s_bench_owner && response.type == KVMCTL_HID_ERROR);
    CHECK(response.request_sequence_present && response.request_sequence == 5);
    usb_cleanup = false;
    usb_held = false;
    service_bench_locked(1, &log);
    CHECK(response.type == KVMCTL_HID_LEASE_STATE && response.request_sequence == 4);
    CHECK(append_fixture());
    event.sequence = 6;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    CHECK(s_bench_owner == slot && !usb_held);
    CHECK(response.type == KVMCTL_HID_LEASE_STATE && response.request_sequence == 6);
    CHECK(append_fixture());
    CHECK(s_lease.epoch == epoch && lease_held_empty(&s_lease));

    int failure = check_drag(owner, &event, 125);
    if (failure) return failure;
    failure = check_drag(owner, &event, 2000);
    if (failure) return failure;
    event.type = KVMCTL_HID_MOUSE_BUTTONS;
    event.button_mask = 1;
    ++event.sequence;
    CHECK(accept_scripted_event(owner, &event, false) == 1);
    complete_usb_report();
    service_bench_locked(1, &log);
    CHECK(s_lease.held_buttons);
    drop_next_motion = true;
    event.type = KVMCTL_HID_MOTION;
    event.button_mask = 1;
    ++event.sequence;
    CHECK(accept_scripted_event(owner, &event, true) == 2);
    CHECK(s_lease.held_buttons);
    event.type = KVMCTL_HID_MOUSE_BUTTONS;
    event.button_mask = 0;
    ++event.sequence;
    CHECK(accept_scripted_event(owner, &event, false) == 1);
    CHECK(!s_lease.held_buttons);
    event.type = KVMCTL_HID_MOTION;
    event.button_mask = 1;
    ++event.sequence;
    CHECK(accept_scripted_event(owner, &event, true) == 1);
    CHECK(!s_lease.held_buttons);
    event.type = KVMCTL_HID_MOUSE_BUTTONS;
    event.button_mask = 1;
    ++event.sequence;
    CHECK(accept_scripted_event(owner, &event, false) == 1);
    CHECK(usb_held && s_lease.held_buttons);
    event.type = KVMCTL_HID_HEARTBEAT;
    ++event.sequence;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    CHECK(s_lease.active && s_lease.epoch == epoch);
    usb_attached = false;
    service_bench_locked(1, &log);
    CHECK(!s_bench_owner && !usb_held && !s_lease.active && lease_held_empty(&s_lease));
    CHECK(response.type == KVMCTL_HID_LEASE_STATE && response.state == 0 && response.lease_epoch != epoch);
    usb_attached = true;
    service_bench_locked(1, &log);
    CHECK(!s_bench_owner && !usb_held);
    event.type = KVMCTL_HID_LIVE_INPUT_ARM;
    ++event.sequence;
    CHECK(accept_scripted_event(owner, &event, false) == 2);
    CHECK(!s_bench_owner && response.type == KVMCTL_HID_ERROR && response.request_sequence == event.sequence);
    return 0;
}
"""


parser = argparse.ArgumentParser()
parser.add_argument("--cc", default="cl" if sys.platform == "win32" else "clang")
parser.add_argument("--fixture", type=Path)
args = parser.parse_args()
build = firmware / "build"
build.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="live-input-test-", dir=build) as temp:
    path = Path(temp)
    source = path / "test.c"
    library = path / ("test.dll" if sys.platform == "win32" else "test.so")
    source.write_text(
        harness.replace("SESSION_AND_DISARM", body("static bool event_session_matches", "static bool pending_requester_eligible_locked"))
        .replace("STATE_REPLY", body("static bool queue_state_reply_locked(", "static void queue_takeover_response_locked")
                 .split("static void queue_state_to_peers_locked")[0]
                 + "static void queue_state_to_peers_locked(void) { queue_state_locked(&s_peers[0], NULL); }\n")
        .replace("ERROR_REPLY", body("static void queue_error_locked", "static void queue_notice_locked"))
        .replace("CURRENT_EVENT_AND_DISPATCH", body("static bool event_is_current_locked", "static void stop_heap_monitor_if_idle"))
        .replace("HID_WORKER", body("static void service_bench_locked(", "static void hid_bench_task("))
        .replace("static bool event_is_current_locked",
                 "static void begin_lease_cleanup_locked(lease_log_snapshot_t *log) "
                 "{ (void)log; bench_disarm_locked(); s_lease_cleanup_pending = true; }\n"
                 "static bool event_is_current_locked")
    )
    inputs = [str(source), str(main / "lease.c"), str(main / "hid_protocol.c")]
    if sys.platform == "win32":
        command = [args.cc, "/nologo", "/std:c11", "/D_CRT_SECURE_NO_WARNINGS",
                   "/LD", "/I" + str(main), *inputs, "/Fe:" + str(library)]
    else:
        command = [args.cc, "-std=c11", "-shared", "-fPIC", "-I", str(main),
                   *inputs, "-o", str(library)]
    subprocess.run(command, cwd=path, check=True)
    loaded = ctypes.CDLL(str(library))
    result = loaded.run_checks()
    if not result and args.fixture:
        size = ctypes.c_size_t()
        loaded.fixture_data.argtypes = [ctypes.POINTER(ctypes.c_size_t)]
        loaded.fixture_data.restype = ctypes.c_void_p
        wire = loaded.fixture_data(ctypes.byref(size))
        args.fixture.write_bytes(ctypes.string_at(wire, size.value))
    if sys.platform == "win32":
        ctypes.windll.kernel32.FreeLibrary(ctypes.c_void_p(loaded._handle))
    if result:
        raise AssertionError(f"live-input regression failed at generated C line {result}")
print("PASS: firmware arm/rearm, USB completion credits, 125 Hz and 2 kHz aggregate drag, and detach")
