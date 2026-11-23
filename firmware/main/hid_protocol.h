#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define KVMCTL_HID_MAX_KEYS 32
#define KVMCTL_HID_MAX_CONTROL 512
#define KVMCTL_HID_MAX_MOTION 64

typedef enum {
    KVMCTL_HID_LEASE_REQUEST = 0x20,
    KVMCTL_HID_TAKEOVER_REQUEST = 0x21,
    KVMCTL_HID_TAKEOVER_RESPONSE = 0x22,
    KVMCTL_HID_LEASE_STATE = 0x23,
    KVMCTL_HID_HEARTBEAT = 0x24,
    KVMCTL_HID_TAKEOVER_NOTICE = 0x25,
    KVMCTL_HID_ERROR = 0x26,
    KVMCTL_HID_LEASE_RELEASE = 0x27,
    KVMCTL_HID_BENCH_ARM = 0x28,
    KVMCTL_HID_KEYBOARD_STATE = 0x30,
    KVMCTL_HID_MOUSE_BUTTONS = 0x31,
    KVMCTL_HID_WHEEL = 0x32,
    KVMCTL_HID_MOTION = 0x40,
} kvmctl_hid_type_t;

typedef struct {
    kvmctl_hid_type_t type;
    uint8_t session_id[16];
    uint32_t lease_epoch;
    uint64_t sequence;
    uint8_t modifier_mask;
    uint8_t key_count;
    uint16_t keys[KVMCTL_HID_MAX_KEYS][2];
    uint8_t button_mask;
    int16_t first_delta;
    int16_t second_delta;
    bool owner_present;
    uint8_t owner[32];
    uint8_t state;
    uint8_t requester[32];
    uint64_t response_window_ms;
    char error_code[33];
    uint8_t request_id[16];
    bool accepted;
} kvmctl_hid_event_t;

bool kvmctl_hid_decode(const uint8_t *data, size_t length, bool motion_channel,
                       kvmctl_hid_event_t *event);
size_t kvmctl_hid_encode(const kvmctl_hid_event_t *event, uint8_t *data,
                         size_t capacity);
bool kvmctl_hid_check_vectors(void);
