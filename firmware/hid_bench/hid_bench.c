#include "hid_bench.h"

#include <stdatomic.h>
#include <string.h>

#include "tinyusb.h"
#include "tinyusb_default_config.h"
#include "class/hid/hid_device.h"
#include "esp_timer.h"

#define REPORT_QUEUE_DEPTH 8
#define SERIAL_BYTES 12
#define CONFIG_DESCRIPTOR_LEN (TUD_CONFIG_DESC_LEN + TUD_HID_DESC_LEN)
#define TRANSFER_TIMEOUT_MS 100
#define CLEANUP_DEADLINE_MS 250
#define CLEANUP_RETRIES 2

typedef struct {
    uint8_t report_id;
    uint8_t modifiers;
    uint8_t keys[6];
    uint8_t buttons;
    int8_t x;
    int8_t y;
    int8_t wheel;
    int8_t pan;
} bench_report_t;

enum {
    TRANSFER_IDLE,
    TRANSFER_PENDING,
    TRANSFER_COMPLETE,
    TRANSFER_FAILED,
};

static const uint8_t s_report_descriptor[] = {
    TUD_HID_REPORT_DESC_KEYBOARD(HID_REPORT_ID(HID_ITF_PROTOCOL_KEYBOARD)),
    TUD_HID_REPORT_DESC_MOUSE(HID_REPORT_ID(HID_ITF_PROTOCOL_MOUSE)),
};

static const uint8_t s_config_descriptor[] = {
    TUD_CONFIG_DESCRIPTOR(1, 1, 0, CONFIG_DESCRIPTOR_LEN,
                          TUSB_DESC_CONFIG_ATT_REMOTE_WAKEUP, 100),
    TUD_HID_DESCRIPTOR(0, 4, false, sizeof(s_report_descriptor), 0x81, 16, 1),
};

static char s_serial[SERIAL_BYTES * 2 + 1];
static const char *s_strings[] = {
    (const char[]){0x09, 0x04},
    "kvmctl",
    "kvmctl scripted HID bench",
    s_serial,
    "Keyboard and mouse",
};

static bench_report_t s_reports[REPORT_QUEUE_DEPTH];
static size_t s_head;
static size_t s_count;
static uint8_t s_queued_buttons;
static uint8_t s_sent_buttons;
static bool s_sent_keys;
static bool s_releasing;
static atomic_bool s_detached;
static bench_report_t s_inflight;
static atomic_int s_transfer;
static atomic_uint s_completed_reports;
static bool s_cleanup_failed;
static uint64_t s_transfer_deadline_ms;
static uint64_t s_cleanup_deadline_ms;
static uint8_t s_cleanup_attempts;
#ifdef KVMCTL_HID_BENCH_FAIL_TRANSFER_ONCE
static bool s_fail_transfer_once;
#endif

static uint64_t now_ms(void)
{
    return (uint64_t)esp_timer_get_time() / 1000ULL;
}

uint8_t const *tud_hid_descriptor_report_cb(uint8_t instance)
{
    (void)instance;
    return s_report_descriptor;
}

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id,
                               hid_report_type_t report_type, uint8_t *buffer,
                               uint16_t requested_len)
{
    (void)instance;
    (void)report_id;
    (void)report_type;
    (void)buffer;
    (void)requested_len;
    return 0;
}

void tud_hid_set_report_cb(uint8_t instance, uint8_t report_id,
                           hid_report_type_t report_type,
                           uint8_t const *buffer, uint16_t buffer_size)
{
    (void)instance;
    (void)report_id;
    (void)report_type;
    (void)buffer;
    (void)buffer_size;
}

void tud_hid_report_complete_cb(uint8_t instance, const uint8_t *report, uint16_t length)
{
    (void)instance;
    (void)report;
    (void)length;
    int expected = TRANSFER_PENDING;
    if (atomic_compare_exchange_strong(&s_transfer, &expected, TRANSFER_COMPLETE))
        atomic_fetch_add(&s_completed_reports, 1);
}

void tud_hid_report_failed_cb(uint8_t instance, hid_report_type_t report_type,
                              const uint8_t *report, uint16_t transferred)
{
    (void)instance;
    (void)report;
    (void)transferred;
    int expected = TRANSFER_PENDING;
    if (report_type == HID_REPORT_TYPE_INPUT)
        (void)atomic_compare_exchange_strong(&s_transfer, &expected, TRANSFER_FAILED);
}

static void usb_event(tinyusb_event_t *event, void *argument)
{
    (void)argument;
    if (event->id == TINYUSB_EVENT_DETACHED) atomic_store(&s_detached, true);
}

esp_err_t hid_bench_init(const uint8_t fingerprint[32])
{
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < SERIAL_BYTES; ++i) {
        s_serial[2 * i] = hex[fingerprint[i] >> 4];
        s_serial[2 * i + 1] = hex[fingerprint[i] & 15];
    }
    tinyusb_config_t config = TINYUSB_DEFAULT_CONFIG();
    config.descriptor.full_speed_config = s_config_descriptor;
    config.descriptor.string = s_strings;
    config.descriptor.string_count = sizeof(s_strings) / sizeof(s_strings[0]);
    config.event_cb = usb_event;
    return tinyusb_driver_install(&config);
}

bool hid_bench_attached(void)
{
    return tud_mounted() && !atomic_load(&s_detached);
}

bool hid_bench_can_arm(void)
{
    return hid_bench_attached() && !s_releasing &&
           !s_cleanup_failed &&
           atomic_load(&s_transfer) == TRANSFER_IDLE;
}

static void push_report(const bench_report_t *report)
{
    s_reports[(s_head + s_count++) % REPORT_QUEUE_DEPTH] = *report;
}

const char *hid_bench_enqueue(const kvmctl_hid_event_t *event)
{
    if (!hid_bench_attached()) return "invalid_capability";
    bench_report_t report = {
        .report_id = HID_ITF_PROTOCOL_MOUSE,
        .buttons = s_queued_buttons,
    };
    bool split_wheel = false;
    switch (event->type) {
    case KVMCTL_HID_KEYBOARD_STATE:
        report.report_id = HID_ITF_PROTOCOL_KEYBOARD;
        report.modifiers = event->modifier_mask;
        if (event->key_count > sizeof(report.keys)) return "invalid_capability";
        for (size_t i = 0; i < event->key_count; ++i) {
            uint16_t usage = event->keys[i][1];
            if (event->keys[i][0] != HID_USAGE_PAGE_KEYBOARD || usage < 4 || usage > UINT8_MAX ||
                (usage >= 0xe0 && usage <= 0xe7)) return "invalid_capability";
            report.keys[i] = (uint8_t)usage;
        }
        break;
    case KVMCTL_HID_MOUSE_BUTTONS:
        report.buttons = event->button_mask;
        break;
    case KVMCTL_HID_WHEEL:
    case KVMCTL_HID_MOTION:
        if (event->first_delta < -127 || event->first_delta > 127 ||
            event->second_delta < -127 || event->second_delta > 127)
            return "invalid_capability";
        if (event->type == KVMCTL_HID_MOTION) {
            report.x = (int8_t)event->first_delta;
            report.y = (int8_t)event->second_delta;
        } else {
            report.wheel = (int8_t)event->first_delta;
            report.pan = (int8_t)event->second_delta;
            // Raw Input has one signed data field for either wheel axis.
            split_wheel = report.wheel && report.pan;
        }
        break;
    default:
        return "invalid_capability";
    }
    if (s_count + 1 + (size_t)split_wheel > REPORT_QUEUE_DEPTH) return "resource_exhausted";
    if (split_wheel) {
        bench_report_t vertical = report;
        vertical.pan = 0;
        push_report(&vertical);
        report.wheel = 0;
    }
    push_report(&report);
    if (event->type == KVMCTL_HID_MOUSE_BUTTONS) s_queued_buttons = report.buttons;
    if (tud_suspended()) (void)tud_remote_wakeup();
    return NULL;
}

void hid_bench_disarm(void)
{
    s_head = 0;
    s_count = 0;
    s_queued_buttons = 0;
    if (!s_releasing && (s_sent_keys || s_sent_buttons != 0 ||
                         atomic_load(&s_transfer) != TRANSFER_IDLE ||
                         s_cleanup_failed)) {
        s_releasing = true;
        s_cleanup_deadline_ms = now_ms() + CLEANUP_DEADLINE_MS;
        s_cleanup_attempts = 0;
    }
}

static bool submit_report(const bench_report_t *report)
{
    s_inflight = *report;
    atomic_store(&s_transfer, TRANSFER_PENDING);
    s_transfer_deadline_ms = now_ms() + TRANSFER_TIMEOUT_MS;
#ifdef KVMCTL_HID_BENCH_FAIL_TRANSFER_ONCE
    if (!s_fail_transfer_once) {
        s_fail_transfer_once = true;
        atomic_store(&s_transfer, TRANSFER_FAILED);
        return true;
    }
#endif
    bool queued = report->report_id == HID_ITF_PROTOCOL_KEYBOARD
        ? tud_hid_keyboard_report(report->report_id, report->modifiers, report->keys)
        : tud_hid_mouse_report(report->report_id, report->buttons, report->x, report->y,
                               report->wheel, report->pan);
    if (!queued) atomic_store(&s_transfer, TRANSFER_FAILED);
    return queued;
}

bool hid_bench_service(bool *detached)
{
    *detached = atomic_exchange(&s_detached, false) || !tud_mounted();
    if (*detached) {
        s_sent_keys = false;
        s_sent_buttons = 0;
        s_releasing = false;
        s_cleanup_failed = false;
        s_cleanup_attempts = 0;
        atomic_store(&s_transfer, TRANSFER_IDLE);
        hid_bench_disarm();
        return true;
    }
    uint64_t now = now_ms();
    int transfer = atomic_load(&s_transfer);
    if (transfer == TRANSFER_PENDING) {
        if (now < s_transfer_deadline_ms) return true;
        atomic_store(&s_transfer, TRANSFER_FAILED);
        transfer = TRANSFER_FAILED;
    }
    if (transfer != TRANSFER_IDLE) {
        bool failed = transfer == TRANSFER_FAILED;
        atomic_store(&s_transfer, TRANSFER_IDLE);
        if (s_inflight.report_id == HID_ITF_PROTOCOL_KEYBOARD) {
            bool held = s_inflight.modifiers != 0 || s_inflight.keys[0] != 0;
            s_sent_keys = held || failed;
        } else {
            if (failed) s_sent_buttons |= s_inflight.buttons;
            else s_sent_buttons = s_inflight.buttons;
        }
        if (failed) {
            if (!s_releasing) {
                s_releasing = true;
                s_cleanup_deadline_ms = now + CLEANUP_DEADLINE_MS;
                s_cleanup_attempts = 0;
            }
            if (s_cleanup_attempts >= CLEANUP_RETRIES || now >= s_cleanup_deadline_ms) {
                s_cleanup_failed = true;
                return false;
            }
        }
        if (!failed && s_releasing) s_releasing = s_sent_keys || s_sent_buttons != 0;
    }
    if (s_releasing && now >= s_cleanup_deadline_ms) {
        s_cleanup_failed = true;
        return false;
    }
    if (!tud_hid_ready()) return true;
    if (s_releasing) {
        const bench_report_t release = {
            .report_id = s_sent_keys ? HID_ITF_PROTOCOL_KEYBOARD : HID_ITF_PROTOCOL_MOUSE,
        };
        ++s_cleanup_attempts;
        (void)submit_report(&release);
    } else if (s_count && submit_report(&s_reports[s_head])) {
        s_head = (s_head + 1) % REPORT_QUEUE_DEPTH;
        --s_count;
    }
    return true;
}

bool hid_bench_cleanup_complete(void)
{
    return !s_releasing && !s_cleanup_failed && s_count == 0 &&
           atomic_load(&s_transfer) == TRANSFER_IDLE;
}

bool hid_bench_releasing(void)
{
    return s_releasing;
}

uint32_t hid_bench_reports(void)
{
    return atomic_load(&s_completed_reports);
}
