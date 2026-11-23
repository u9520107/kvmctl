#include "transport.h"
#include "hid_protocol.h"
#include "lease.h"

#include <stdbool.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdatomic.h>
#include <string.h>

#include "client_auth.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_peer_default.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"
#include "mbedtls/platform_util.h"
#include "wifi_setup.h"

/* Provided by the pinned ESP peer fingerprint patch. */
extern bool esp_peer_remote_fingerprint_rejected(esp_peer_handle_t peer);

#define MAX_TRANSPORT_PEERS 2
#define SDP_CAPACITY 8192
#define CONTROL_CAPACITY 512
#define MOTION_CAPACITY 64
#define RESPONSE_QUEUE_DEPTH 4
#define BIT_ANSWER BIT0
#define BIT_FAILED BIT1
#define BIT_CLOSED BIT2
#define BIT_CERT_READY BIT3

typedef struct {
    uint16_t stream;
    bool open;
    bool pending;
    uint8_t data[CONTROL_CAPACITY];
    esp_peer_data_frame_t echo;
} echo_channel_t;

typedef struct {
    uint8_t data[RESPONSE_QUEUE_DEPTH][CONTROL_CAPACITY];
    uint16_t size[RESPONSE_QUEUE_DEPTH];
    uint8_t head;
    uint8_t count;
} response_queue_t;

typedef struct {
    EventGroupHandle_t events;
    atomic_bool active;
    atomic_bool close_requested;
    atomic_bool failed;
    atomic_bool identity_rejected;
    atomic_bool application_authorized;
    uint8_t client_fingerprint[32];
    uint32_t generation;
    uint8_t offer[SDP_CAPACITY + 1];
    uint8_t answer[SDP_CAPACITY + 1];
    size_t answer_len;
    echo_channel_t control;
    echo_channel_t motion;
    response_queue_t responses;
    atomic_uchar open_channels;
    uint64_t control_sequence;
    uint64_t response_sequence;
    uint64_t motion_sequence;
    bool motion_sequence_seen;
    bool session_id_seen;
    uint8_t session_id[16];
    uint32_t peer_task_cpu_us;
} transport_peer_t;

static transport_peer_t s_peers[MAX_TRANSPORT_PEERS];
static EventGroupHandle_t s_certificate_events;
static portMUX_TYPE s_peer_mux = portMUX_INITIALIZER_UNLOCKED;
static atomic_uint s_active_peers;
static atomic_uint s_generation_counter;
static atomic_bool s_heap_monitor;
static atomic_uint s_open_channels;
static atomic_uint s_channel_opens;
static atomic_uint s_application_bytes_received;
static atomic_uint s_peer_task_cpu_us;
static atomic_uint s_tls_rate_limited;
static atomic_uint s_scripted_events;
static atomic_uint s_scripted_rejections;
static atomic_uint s_scripted_drops;
static atomic_uint s_scripted_reorders;
static atomic_ullong s_last_event_timestamp_ms;
static atomic_ullong s_last_event_sequence;
static atomic_uint s_internal_free_start;
static atomic_uint s_internal_free_min;
static atomic_uint s_internal_free_end;
static atomic_uint s_psram_free_start;
static atomic_uint s_psram_free_min;
static atomic_uint s_psram_free_end;
static portMUX_TYPE s_tls_diagnostic_mux = portMUX_INITIALIZER_UNLOCKED;
static char s_tls_diagnostic[TRANSPORT_DIAGNOSTIC_CAPACITY];
static uint8_t s_tls_diagnostic_priority;

/* Endpoint-global lease: one owner, one pending requester. */
static lease_state_t s_lease;
static portMUX_TYPE s_lease_mux = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_response_mux[MAX_TRANSPORT_PEERS] = {
    portMUX_INITIALIZER_UNLOCKED, portMUX_INITIALIZER_UNLOCKED,
};

static bool queue_response(transport_peer_t *slot, const kvmctl_hid_event_t *event);
static void queue_state_locked(transport_peer_t *slot);
static void queue_state_to_peers_locked(void);

static int peer_index(const transport_peer_t *slot)
{
    if (!slot) return -1;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (slot == &s_peers[i]) return (int)i;
    }
    return -1;
}

static const uint8_t *lease_slot_identity(int slot)
{
    return slot >= 0 && slot < MAX_TRANSPORT_PEERS ? s_peers[slot].client_fingerprint : NULL;
}

static bool event_session_matches(transport_peer_t *slot, const kvmctl_hid_event_t *event)
{
    if (!slot || !event) return false;
    if (!slot->session_id_seen) return true;
    return memcmp(slot->session_id, event->session_id, sizeof(slot->session_id)) == 0;
}

static void lease_tick_locked(uint64_t now_ms)
{
    lease_result_t result = lease_tick(&s_lease, now_ms);
    if (result == LEASE_EXPIRED || result == LEASE_TAKEOVER_GRANTED) {
        queue_state_to_peers_locked();
    }
}

static bool queue_response(transport_peer_t *slot, const kvmctl_hid_event_t *event)
{
    int peer = peer_index(slot);
    if (peer < 0 || !event) return false;
    bool queued = false;
    portENTER_CRITICAL(&s_response_mux[peer]);
    if (slot->control.open && atomic_load(&slot->application_authorized) &&
        slot->responses.count < RESPONSE_QUEUE_DEPTH) {
        uint8_t index = (uint8_t)((slot->responses.head + slot->responses.count) % RESPONSE_QUEUE_DEPTH);
        kvmctl_hid_event_t copy = *event;
        copy.sequence = slot->response_sequence++;
        size_t length = kvmctl_hid_encode(&copy, slot->responses.data[index], CONTROL_CAPACITY);
        if (length) {
            slot->responses.size[index] = (uint16_t)length;
            ++slot->responses.count;
            queued = true;
        }
    }
    portEXIT_CRITICAL(&s_response_mux[peer]);
    return queued;
}

static void queue_state_locked(transport_peer_t *slot)
{
    if (!slot || !slot->session_id_seen) return;
    kvmctl_hid_event_t state = {0};
    state.type = KVMCTL_HID_LEASE_STATE;
    memcpy(state.session_id, slot->session_id, sizeof(state.session_id));
    state.lease_epoch = s_lease.epoch;
    state.owner_present = s_lease.active;
    if (state.owner_present) memcpy(state.owner, s_lease.owner, sizeof(state.owner));
    state.state = s_lease.pending ? 2 : s_lease.active ? 1 : 0;
    (void)queue_response(slot, &state);
}

static void queue_state_to_peers_locked(void)
{
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (atomic_load(&s_peers[i].active) &&
            atomic_load(&s_peers[i].application_authorized) &&
            s_peers[i].session_id_seen) {
            queue_state_locked(&s_peers[i]);
        }
    }
}

static void queue_takeover_response_locked(const uint8_t *requester,
                                           const uint8_t *request_id,
                                           bool accepted)
{
    if (!requester || !request_id) return;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (!atomic_load(&s_peers[i].active) ||
            !atomic_load(&s_peers[i].application_authorized) ||
            !s_peers[i].session_id_seen ||
            memcmp(s_peers[i].client_fingerprint, requester, LEASE_ID_SIZE) != 0) continue;
        kvmctl_hid_event_t response = {0};
        response.type = KVMCTL_HID_TAKEOVER_RESPONSE;
        memcpy(response.session_id, s_peers[i].session_id, sizeof(response.session_id));
        response.lease_epoch = s_lease.epoch;
        memcpy(response.request_id, request_id, sizeof(response.request_id));
        response.accepted = accepted;
        (void)queue_response(&s_peers[i], &response);
        return;
    }
}

static void queue_error_locked(transport_peer_t *slot, const kvmctl_hid_event_t *event, const char *code)
{
    if (!slot || !event || !slot->session_id_seen) return;
    kvmctl_hid_event_t error = {0};
    error.type = KVMCTL_HID_ERROR;
    memcpy(error.session_id, slot->session_id, sizeof(error.session_id));
    error.lease_epoch = s_lease.epoch;
    strncpy(error.error_code, code, sizeof(error.error_code) - 1);
    (void)queue_response(slot, &error);
}

static void queue_notice_locked(transport_peer_t *slot, const kvmctl_hid_event_t *request)
{
    if (!slot || !request || !s_lease.active) return;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (!atomic_load(&s_peers[i].active) || !atomic_load(&s_peers[i].application_authorized) || !s_peers[i].session_id_seen) continue;
        if (memcmp(s_peers[i].client_fingerprint, s_lease.owner, LEASE_ID_SIZE) != 0) continue;
        kvmctl_hid_event_t notice = {0};
        notice.type = KVMCTL_HID_TAKEOVER_NOTICE;
        memcpy(notice.session_id, s_peers[i].session_id, sizeof(notice.session_id));
        notice.lease_epoch = s_lease.epoch;
        memcpy(notice.request_id, request->request_id, sizeof(notice.request_id));
        memcpy(notice.requester, lease_slot_identity(peer_index(slot)), sizeof(notice.requester));
        notice.response_window_ms = LEASE_TAKEOVER_WINDOW_MS;
        (void)queue_response(&s_peers[i], &notice);
    }
}

static bool event_is_current_locked(int slot, const kvmctl_hid_event_t *event)
{
    return lease_can_emit(&s_lease, lease_slot_identity(slot), event->lease_epoch);
}

/* 1 apply, 0 stale/drop, -1 reject. */
static int accept_scripted_event(int slot, const kvmctl_hid_event_t *event)
{
    if (!event || slot < 0 || slot >= MAX_TRANSPORT_PEERS) return -1;
    if (!event_session_matches(&s_peers[slot], event)) return -1;
    uint64_t now = (uint64_t)esp_timer_get_time() / 1000ULL;
    int result = -1;
    lease_result_t lease_result = LEASE_REJECTED;
    portENTER_CRITICAL(&s_lease_mux);
    lease_tick_locked(now);
    if (!s_peers[slot].session_id_seen) {
        memcpy(s_peers[slot].session_id, event->session_id, sizeof(s_peers[slot].session_id));
        s_peers[slot].session_id_seen = true;
    }
    switch (event->type) {
    case KVMCTL_HID_LEASE_REQUEST:
        if (event->lease_epoch != 0) { queue_error_locked(&s_peers[slot], event, "stale_epoch"); result = 2; break; }
        lease_result = lease_request(&s_lease, lease_slot_identity(slot), NULL, now);
        if (lease_result == LEASE_GRANTED) { queue_state_locked(&s_peers[slot]); result = 2; }
        else { queue_error_locked(&s_peers[slot], event, "busy"); result = 2; }
        break;
    case KVMCTL_HID_HEARTBEAT:
        lease_result = lease_heartbeat(&s_lease, lease_slot_identity(slot), event->lease_epoch, now);
        if (lease_result == LEASE_HEARTBEAT_ACCEPTED) result = 2;
        else { queue_error_locked(&s_peers[slot], event, s_lease.active ? "stale_epoch" : "lease_expired"); result = 2; }
        break;
    case KVMCTL_HID_TAKEOVER_REQUEST:
        lease_result = lease_request(&s_lease, lease_slot_identity(slot), event->request_id, now);
        if (lease_result == LEASE_TAKEOVER_PENDING) { queue_notice_locked(&s_peers[slot], event); queue_state_locked(&s_peers[slot]); result = 2; }
        else if (lease_result == LEASE_GRANTED) { queue_state_locked(&s_peers[slot]); result = 2; }
        else { queue_error_locked(&s_peers[slot], event, "busy"); result = 2; }
        break;
    case KVMCTL_HID_TAKEOVER_RESPONSE: {
        bool had_pending = s_lease.pending;
        uint8_t requester[LEASE_ID_SIZE] = {0};
        if (had_pending) memcpy(requester, s_lease.pending_requester, sizeof(requester));
        lease_result = lease_takeover_response(&s_lease, lease_slot_identity(slot),
                                               event->lease_epoch, event->request_id,
                                               event->accepted);
        result = event->accepted ? lease_result == LEASE_TAKEOVER_ACCEPTED
                                 : had_pending && lease_result == LEASE_REJECTED && !s_lease.pending;
        if (result && event->accepted) {
            queue_takeover_response_locked(requester, event->request_id, true);
        } else if (result) {
            queue_takeover_response_locked(requester, event->request_id, false);
            queue_state_to_peers_locked();
        }
        else { queue_error_locked(&s_peers[slot], event, "unauthorized"); result = 2; }
        if (event->accepted) result = 2;
        break;
    }
    case KVMCTL_HID_LEASE_RELEASE:
        lease_result = lease_release(&s_lease, lease_slot_identity(slot), event->lease_epoch, now);
        if (lease_result == LEASE_TAKEOVER_GRANTED || lease_result == LEASE_RELEASED) { queue_state_to_peers_locked(); result = 2; }
        else { queue_error_locked(&s_peers[slot], event, s_lease.active ? "stale_epoch" : "lease_expired"); result = 2; }
        break;
    case KVMCTL_HID_KEYBOARD_STATE:
    case KVMCTL_HID_MOUSE_BUTTONS:
    case KVMCTL_HID_WHEEL:
    case KVMCTL_HID_MOTION:
        result = event_is_current_locked(slot, event) ? 1 : 2;
        if (result == 2) queue_error_locked(&s_peers[slot], event, s_lease.active ? "stale_epoch" : "lease_expired");
        if (result == 1 && event->type == KVMCTL_HID_KEYBOARD_STATE) {
            (void)lease_set_held(&s_lease, lease_slot_identity(slot), event->lease_epoch,
                                 event->key_count != 0 || event->modifier_mask != 0,
                                 s_lease.held_buttons);
        } else if (result == 1 && event->type == KVMCTL_HID_MOUSE_BUTTONS) {
            (void)lease_set_held(&s_lease, lease_slot_identity(slot), event->lease_epoch,
                                 s_lease.held_keys, event->button_mask != 0);
        }
        break;
    default:
        break;
    }
    if (result > 0) {
        atomic_store(&s_last_event_timestamp_ms, now);
        atomic_store(&s_last_event_sequence, event->sequence);
    }
    portEXIT_CRITICAL(&s_lease_mux);
    if (result == 0) atomic_fetch_add(&s_scripted_drops, 1);
    return result;
}

static void stop_heap_monitor_if_idle(void)
{
    if (atomic_load(&s_active_peers) != 0 || !atomic_load(&s_heap_monitor)) return;
    atomic_store(&s_internal_free_min, heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL));
    atomic_store(&s_internal_free_end, heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
    atomic_store(&s_psram_free_min, heap_caps_get_minimum_free_size(MALLOC_CAP_SPIRAM));
    atomic_store(&s_psram_free_end, heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
    heap_caps_monitor_local_minimum_free_size_stop();
    atomic_store(&s_heap_monitor, false);
}

static int capture_peer_log(const char *format, va_list args)
{
    char message[256];
    va_list copy;
    va_copy(copy, args);
    int length = vsnprintf(message, sizeof(message), format, copy);
    va_end(copy);
    if (length > 0 && !strstr(message, "esp_https_server") &&
                      (strstr(message, "esp-tls") || strstr(message, "ESP-TLS") ||
                       strstr(message, "esp_tls") || strstr(message, "https_server") ||
                       strstr(message, "mbedtls") || strstr(message, "TLS"))) {
        uint8_t priority = strstr(message, "kvmctl_signal") ? 1 : 0;
        const char *diagnostic = message;
        const char *reason = strstr(message, "certificate mismatch:");
        if (!reason) reason = strstr(message, "server has no certificate");
        if (!reason) reason = strstr(message, "ciphersuite mismatch:");
        if (reason) { diagnostic = reason; priority = 4; }
        size_t copy_len = strnlen(diagnostic, sizeof(s_tls_diagnostic) - 1);
        while (copy_len && (diagnostic[copy_len - 1] == '\r' || diagnostic[copy_len - 1] == '\n')) --copy_len;
        portENTER_CRITICAL(&s_tls_diagnostic_mux);
        if (priority >= s_tls_diagnostic_priority) {
            memcpy(s_tls_diagnostic, diagnostic, copy_len);
            s_tls_diagnostic[copy_len] = '\0';
            s_tls_diagnostic_priority = priority;
        }
        portEXIT_CRITICAL(&s_tls_diagnostic_mux);
    }
    return 0;
}

void transport_silence_logs(void) { esp_log_set_vprintf(capture_peer_log); }
void transport_note_tls_rate_limited(void) { atomic_fetch_add(&s_tls_rate_limited, 1); }
void transport_clear_tls_diagnostic(void)
{
    portENTER_CRITICAL(&s_tls_diagnostic_mux);
    memset(s_tls_diagnostic, 0, sizeof(s_tls_diagnostic));
    s_tls_diagnostic_priority = 0;
    portEXIT_CRITICAL(&s_tls_diagnostic_mux);
}

static void fail_session(transport_peer_t *slot)
{
    atomic_store(&slot->failed, true);
    xEventGroupSetBits(slot->events, BIT_FAILED);
}

static int on_state(esp_peer_state_t state, void *ctx)
{
    transport_peer_t *slot = ctx;
    if (slot && !atomic_load(&slot->close_requested) &&
        (state == ESP_PEER_STATE_CONNECT_FAILED || state == ESP_PEER_STATE_DATA_CHANNEL_DISCONNECTED)) fail_session(slot);
    return 0;
}

static int on_message(esp_peer_msg_t *message, void *ctx)
{
    transport_peer_t *slot = ctx;
    if (!slot || !message || message->type != ESP_PEER_MSG_TYPE_SDP || !message->data ||
        message->size <= 0 || message->size > SDP_CAPACITY || slot->answer_len != 0) {
        if (slot) fail_session(slot);
        return -1;
    }
    memcpy(slot->answer, message->data, (size_t)message->size);
    slot->answer_len = (size_t)message->size;
    xEventGroupSetBits(slot->events, BIT_ANSWER);
    return 0;
}

static int on_channel(esp_peer_data_channel_info_t *channel, void *ctx)
{
    transport_peer_t *slot = ctx;
    if (!slot || !channel || !channel->label) { if (slot) fail_session(slot); return -1; }
    echo_channel_t *target = strcmp(channel->label, "control") == 0 ? &slot->control :
                             strcmp(channel->label, "motion") == 0 ? &slot->motion : NULL;
    if (!target || target->open || (slot->control.open && channel->stream_id == slot->control.stream) ||
        (slot->motion.open && channel->stream_id == slot->motion.stream)) { fail_session(slot); return -1; }
    target->stream = channel->stream_id;
    target->open = true;
    ++slot->open_channels;
    atomic_fetch_add(&s_open_channels, 1);
    atomic_fetch_add(&s_channel_opens, 1);
    return 0;
}

static int on_channel_close(esp_peer_data_channel_info_t *channel, void *ctx)
{
    transport_peer_t *slot = ctx;
    (void)channel;
    if (slot && !atomic_load(&slot->close_requested)) fail_session(slot);
    return 0;
}

static int on_data(esp_peer_data_frame_t *frame, void *ctx)
{
    transport_peer_t *slot = ctx;
    if (!slot || !atomic_load(&slot->application_authorized)) return 0;
    if (!frame || !frame->data) { if (slot) fail_session(slot); return -1; }
    bool motion_channel = slot->motion.open && frame->stream_id == slot->motion.stream;
    bool control_channel = slot->control.open && frame->stream_id == slot->control.stream;
    if (!control_channel && !motion_channel) { fail_session(slot); return -1; }
    if (frame->size > 0 && (frame->data[0] >> 5) == 5) {
        kvmctl_hid_event_t event;
        if (!kvmctl_hid_decode(frame->data, (size_t)frame->size, motion_channel, &event)) {
            atomic_fetch_add(&s_scripted_rejections, 1); fail_session(slot); return -1;
        }
        if ((control_channel && event.sequence != slot->control_sequence) ||
            (motion_channel && slot->motion_sequence_seen && event.sequence <= slot->motion_sequence)) {
            atomic_fetch_add(&s_scripted_drops, 1);
            atomic_fetch_add(&s_scripted_reorders, 1);
            return 0;
        }
        int accepted = accept_scripted_event((int)(slot - s_peers), &event);
        if (accepted < 0) { atomic_fetch_add(&s_scripted_rejections, 1); fail_session(slot); return -1; }
        if (accepted == 0) return 0;
        if (control_channel) ++slot->control_sequence;
        if (motion_channel) { slot->motion_sequence = event.sequence; slot->motion_sequence_seen = true; }
        atomic_fetch_add(&s_scripted_events, 1);
        atomic_fetch_add(&s_application_bytes_received, (uint32_t)frame->size);
        if (accepted == 2 && event.type >= KVMCTL_HID_KEYBOARD_STATE) {
            atomic_fetch_add(&s_scripted_drops, 1);
        }
        if (accepted == 2) return 0;
        /* Scripted CBOR is input, not an application echo. */
        return 0;
    }
    echo_channel_t *target = control_channel ? &slot->control : &slot->motion;
    size_t capacity = control_channel ? CONTROL_CAPACITY : MOTION_CAPACITY;
    if (target->pending || frame->size <= 0 || (size_t)frame->size > capacity) { fail_session(slot); return -1; }
    memcpy(target->data, frame->data, (size_t)frame->size);
    atomic_fetch_add(&s_application_bytes_received, (uint32_t)frame->size);
    target->echo = *frame;
    target->echo.data = target->data;
    target->pending = true;
    return 0;
}

static bool send_echo(esp_peer_handle_t peer, echo_channel_t *channel)
{
    if (!channel->pending) return true;
    int ret = esp_peer_send_data(peer, &channel->echo);
    if (ret == ESP_PEER_ERR_WOULD_BLOCK) return true;
    if (ret != ESP_PEER_ERR_NONE) return false;
    channel->pending = false;
    return true;
}

static bool send_response(esp_peer_handle_t peer, transport_peer_t *slot)
{
    int peer_index_value = peer_index(slot);
    if (peer_index_value < 0) return false;
    uint8_t data[CONTROL_CAPACITY];
    uint16_t size = 0;
    uint8_t index = 0;
    portENTER_CRITICAL(&s_response_mux[peer_index_value]);
    if (slot->responses.count) {
        index = slot->responses.head;
        size = slot->responses.size[index];
        memcpy(data, slot->responses.data[index], size);
    }
    portEXIT_CRITICAL(&s_response_mux[peer_index_value]);
    if (!size) return true;

    esp_peer_data_frame_t frame = {.stream_id = slot->control.stream, .data = data, .size = size};
    int ret = esp_peer_send_data(peer, &frame);
    if (ret == ESP_PEER_ERR_WOULD_BLOCK) return true;
    if (ret != ESP_PEER_ERR_NONE) return false;

    portENTER_CRITICAL(&s_response_mux[peer_index_value]);
    if (slot->responses.count && slot->responses.head == index &&
        slot->responses.size[index] == size) {
        slot->responses.head = (uint8_t)((index + 1) % RESPONSE_QUEUE_DEPTH);
        --slot->responses.count;
    }
    portEXIT_CRITICAL(&s_response_mux[peer_index_value]);
    return true;
}

static void peer_cleanup(transport_peer_t *slot)
{
    portENTER_CRITICAL(&s_lease_mux);
    lease_result_t revoke_result = LEASE_NOOP;
    if (s_lease.active && memcmp(s_lease.owner, slot->client_fingerprint, LEASE_ID_SIZE) == 0) {
        revoke_result = lease_revoke(&s_lease, slot->client_fingerprint,
                                     (uint64_t)esp_timer_get_time() / 1000ULL);
    } else if (s_lease.pending &&
               memcmp(s_lease.pending_requester, slot->client_fingerprint, LEASE_ID_SIZE) == 0) {
        revoke_result = lease_revoke(&s_lease, slot->client_fingerprint,
                                     (uint64_t)esp_timer_get_time() / 1000ULL);
    }
    if (revoke_result == LEASE_TAKEOVER_GRANTED || revoke_result == LEASE_REVOKED) {
        queue_state_to_peers_locked();
    }
    portEXIT_CRITICAL(&s_lease_mux);
    atomic_fetch_sub(&s_open_channels, slot->open_channels);
    memset(slot->offer, 0, sizeof(slot->offer));
    memset(slot->answer, 0, sizeof(slot->answer));
    memset(&slot->control, 0, sizeof(slot->control));
    memset(&slot->motion, 0, sizeof(slot->motion));
    int response_peer = peer_index(slot);
    if (response_peer >= 0) {
        portENTER_CRITICAL(&s_response_mux[response_peer]);
        memset(&slot->responses, 0, sizeof(slot->responses));
        portEXIT_CRITICAL(&s_response_mux[response_peer]);
    }
    slot->answer_len = 0;
    slot->open_channels = 0;
    mbedtls_platform_zeroize(slot->client_fingerprint, sizeof(slot->client_fingerprint));
    mbedtls_platform_zeroize(slot->session_id, sizeof(slot->session_id));
    slot->session_id_seen = false;
    atomic_store(&slot->active, false);
    atomic_fetch_sub(&s_active_peers, 1);
    stop_heap_monitor_if_idle();
    xEventGroupSetBits(slot->events, BIT_CLOSED);
}

static void peer_task(void *arg)
{
    transport_peer_t *slot = arg;
    TaskHandle_t task = xTaskGetCurrentTaskHandle();
    TaskStatus_t info;
    vTaskGetInfo(task, &info, pdFALSE, eInvalid);
    uint32_t runtime_start = (uint32_t)info.ulRunTimeCounter;
    esp_peer_handle_t peer = NULL;
    esp_peer_default_cfg_t defaults = {.agent_recv_timeout = 200, .keep_role = true,
        .data_ch_cfg = {.send_cache_size = 4096, .recv_cache_size = 4096}};
    esp_peer_cfg_t config = {
        .role = ESP_PEER_ROLE_CONTROLLED, .audio_dir = ESP_PEER_MEDIA_DIR_NONE,
        .video_dir = ESP_PEER_MEDIA_DIR_NONE, .enable_data_channel = true,
        .manual_ch_create = true, .no_auto_reconnect = true,
        .on_state = on_state, .on_msg = on_message, .on_data = on_data,
        .on_channel_open = on_channel, .on_channel_close = on_channel_close,
        .ctx = slot, .extra_cfg = &defaults, .extra_size = sizeof(defaults),
    };
    int ret = esp_peer_open(&config, esp_peer_get_default_impl(), &peer);
    if (ret == ESP_PEER_ERR_NONE) ret = esp_peer_new_connection(peer);
    if (ret == ESP_PEER_ERR_NONE) {
        esp_peer_msg_t offer = {.type = ESP_PEER_MSG_TYPE_SDP, .data = slot->offer,
                                .size = (int)strlen((char *)slot->offer)};
        ret = esp_peer_send_msg(peer, &offer);
        if (ret == ESP_PEER_ERR_INVALID_ARG) atomic_store(&slot->identity_rejected, true);
    }
    if (ret != ESP_PEER_ERR_NONE) {
        atomic_store(&slot->failed, true);
        xEventGroupSetBits(slot->events, BIT_FAILED);
    } else {
        while (!atomic_load(&slot->close_requested) && !atomic_load(&slot->failed)) {
            esp_peer_main_loop(peer);
            if (esp_peer_remote_fingerprint_rejected(peer)) break;
            portENTER_CRITICAL(&s_lease_mux);
            lease_tick_locked((uint64_t)esp_timer_get_time() / 1000ULL);
            portEXIT_CRITICAL(&s_lease_mux);
            if (!send_response(peer, slot) || !send_echo(peer, &slot->control) || !send_echo(peer, &slot->motion)) fail_session(slot);
            vTaskDelay(pdMS_TO_TICKS(10));
        }
    }
    if (peer && esp_peer_remote_fingerprint_rejected(peer)) {
        atomic_store(&slot->identity_rejected, true);
        fail_session(slot);
    }
    if (peer) esp_peer_close(peer);
    vTaskDelay(1);
    vTaskGetInfo(task, &info, pdFALSE, eInvalid);
    slot->peer_task_cpu_us = (uint32_t)info.ulRunTimeCounter - runtime_start;
    atomic_store(&s_peer_task_cpu_us, slot->peer_task_cpu_us);
    peer_cleanup(slot);
    vTaskDelete(NULL);
}

static void certificate_task(void *arg)
{
    (void)arg;
    int result = esp_peer_pre_generate_cert();
    xEventGroupSetBits(s_certificate_events, result == ESP_PEER_ERR_NONE ? BIT_CERT_READY : BIT_FAILED);
    vTaskDelete(NULL);
}

esp_err_t transport_init(void)
{
    lease_init(&s_lease);
    s_certificate_events = xEventGroupCreate();
    if (!s_certificate_events) return ESP_ERR_NO_MEM;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        s_peers[i].events = xEventGroupCreate();
        if (!s_peers[i].events) return ESP_ERR_NO_MEM;
    }
    if (xTaskCreate(certificate_task, "peer_cert", 24 * 1024, NULL, 4, NULL) != pdPASS) return ESP_ERR_NO_MEM;
    EventBits_t bits = xEventGroupWaitBits(s_certificate_events, BIT_CERT_READY | BIT_FAILED,
                                           pdFALSE, pdFALSE, pdMS_TO_TICKS(30000));
    return (bits & BIT_CERT_READY) ? ESP_OK : ESP_FAIL;
}

static transport_peer_t *find_slot(const uint8_t *fingerprint, uint32_t generation)
{
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        transport_peer_t *slot = &s_peers[i];
        if (atomic_load(&slot->active) && slot->generation == generation &&
            memcmp(slot->client_fingerprint, fingerprint, 32) == 0) return slot;
    }
    return NULL;
}

static esp_err_t transport_offer_internal(const uint8_t *client_fingerprint,
                                          const uint8_t *session_id,
                                          const uint8_t *offer, size_t offer_len,
                                          uint8_t *answer, size_t answer_capacity,
                                          size_t *answer_len, uint32_t *generation)
{
    if (!s_certificate_events || !offer || !answer || !answer_len || offer_len == 0 ||
        offer_len > SDP_CAPACITY || offer_len < 5 || memcmp(offer, "v=0\r\n", 5) != 0 ||
        memchr(offer, 0, offer_len)) return ESP_ERR_INVALID_ARG;
    if (wifi_setup_state() != 2) return ESP_ERR_INVALID_STATE;
    for (size_t i = 0; i < offer_len; ++i) {
        if ((offer[i] < 0x20 && offer[i] != '\r' && offer[i] != '\n' && offer[i] != '\t') || offer[i] > 0x7e) return ESP_ERR_INVALID_ARG;
    }
    if (client_fingerprint && !client_auth_is_enrolled(client_fingerprint)) return ESP_ERR_INVALID_STATE;
    if (session_id) {
        uint8_t any_session_byte = 0;
        for (size_t i = 0; i < 16; ++i) any_session_byte |= session_id[i];
        if (any_session_byte == 0) return ESP_ERR_INVALID_ARG;
    }
    transport_peer_t *slot = NULL;
    portENTER_CRITICAL(&s_peer_mux);
    if (client_fingerprint) {
        for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
            if (atomic_load(&s_peers[i].active) &&
                memcmp(s_peers[i].client_fingerprint, client_fingerprint, 32) == 0) {
                portEXIT_CRITICAL(&s_peer_mux);
                return ESP_ERR_INVALID_STATE;
            }
        }
    }
    bool any_active = false;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (atomic_load(&s_peers[i].active)) any_active = true;
        else if (!slot) slot = &s_peers[i];
    }
    if (slot) {
        /* Keep terminal flags available to USB until a new session starts. */
        if (!any_active) {
            for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
                atomic_store(&s_peers[i].failed, false);
                atomic_store(&s_peers[i].identity_rejected, false);
            }
        }
        atomic_store(&slot->failed, false);
        atomic_store(&slot->identity_rejected, false);
        slot->generation = ++s_generation_counter;
        if (slot->generation == 0) slot->generation = ++s_generation_counter;
        memset(slot->client_fingerprint, 0, sizeof(slot->client_fingerprint));
        if (client_fingerprint) memcpy(slot->client_fingerprint, client_fingerprint, 32);
        atomic_fetch_add(&s_active_peers, 1);
        atomic_store(&slot->active, true);
    }
    portEXIT_CRITICAL(&s_peer_mux);
    if (!slot) return ESP_ERR_INVALID_STATE;
    bool first_peer = atomic_load(&s_active_peers) == 1;
    if (first_peer) {
        if (heap_caps_monitor_local_minimum_free_size_start() != ESP_OK) {
            atomic_store(&slot->active, false); atomic_fetch_sub(&s_active_peers, 1); return ESP_ERR_INVALID_STATE;
        }
        atomic_store(&s_heap_monitor, true);
        atomic_store(&s_internal_free_start, heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
        atomic_store(&s_internal_free_min, atomic_load(&s_internal_free_start));
        atomic_store(&s_internal_free_end, 0);
        atomic_store(&s_psram_free_start, heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
        atomic_store(&s_psram_free_min, atomic_load(&s_psram_free_start));
        atomic_store(&s_psram_free_end, 0);
        atomic_store(&s_channel_opens, 0);
        atomic_store(&s_application_bytes_received, 0);
        atomic_store(&s_scripted_events, 0);
        atomic_store(&s_scripted_rejections, 0);
        atomic_store(&s_scripted_drops, 0);
        atomic_store(&s_scripted_reorders, 0);
        atomic_store(&s_last_event_timestamp_ms, 0);
        atomic_store(&s_last_event_sequence, 0);
    }
    xEventGroupClearBits(slot->events, BIT_ANSWER | BIT_FAILED | BIT_CLOSED);
    atomic_store(&slot->close_requested, false);
    atomic_store(&slot->application_authorized, false);
    slot->answer_len = 0;
    slot->open_channels = 0;
    slot->control_sequence = 0;
    slot->motion_sequence = 0;
    slot->motion_sequence_seen = false;
    slot->session_id_seen = false;
    memset(slot->session_id, 0, sizeof(slot->session_id));
    if (session_id) {
        memcpy(slot->session_id, session_id, sizeof(slot->session_id));
        slot->session_id_seen = true;
    }
    memset(&slot->control, 0, sizeof(slot->control));
    memset(&slot->motion, 0, sizeof(slot->motion));
    int response_peer = peer_index(slot);
    if (response_peer >= 0) {
        portENTER_CRITICAL(&s_response_mux[response_peer]);
        slot->response_sequence = 0;
        memset(&slot->responses, 0, sizeof(slot->responses));
        portEXIT_CRITICAL(&s_response_mux[response_peer]);
    }
    memcpy(slot->offer, offer, offer_len);
    slot->offer[offer_len] = 0;
    if (xTaskCreate(peer_task, "webrtc_peer", 24 * 1024, slot, 5, NULL) != pdPASS) {
        atomic_store(&slot->active, false); atomic_fetch_sub(&s_active_peers, 1); stop_heap_monitor_if_idle(); return ESP_ERR_NO_MEM;
    }
    EventBits_t bits = xEventGroupWaitBits(slot->events, BIT_ANSWER | BIT_FAILED,
                                           pdFALSE, pdFALSE, pdMS_TO_TICKS(30000));
    if (!(bits & BIT_ANSWER) || atomic_load(&slot->failed)) {
        atomic_store(&slot->close_requested, true);
        xEventGroupWaitBits(slot->events, BIT_CLOSED, pdFALSE, pdTRUE, pdMS_TO_TICKS(5000));
        return atomic_load(&slot->identity_rejected) ? ESP_ERR_INVALID_ARG : ESP_ERR_INVALID_RESPONSE;
    }
    if (slot->answer_len > answer_capacity) { atomic_store(&slot->close_requested, true); return ESP_ERR_INVALID_SIZE; }
    memcpy(answer, slot->answer, slot->answer_len);
    *answer_len = slot->answer_len;
    if (generation) *generation = slot->generation;
    return ESP_OK;
}

esp_err_t transport_offer(const uint8_t *offer, size_t offer_len, uint8_t *answer, size_t answer_capacity, size_t *answer_len)
{ return transport_offer_internal(NULL, NULL, offer, offer_len, answer, answer_capacity, answer_len, NULL); }

esp_err_t transport_offer_for_client(const uint8_t fingerprint[32], const uint8_t session_id[16],
                                     const uint8_t *offer, size_t offer_len,
                                     uint8_t *answer, size_t answer_capacity, size_t *answer_len, uint32_t *generation)
{ return fingerprint && session_id && generation && client_auth_is_enrolled(fingerprint) ? transport_offer_internal(fingerprint, session_id, offer, offer_len, answer, answer_capacity, answer_len, generation) : ESP_ERR_INVALID_STATE; }

esp_err_t transport_close(void)
{
    esp_err_t result = ESP_OK;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (!atomic_load(&s_peers[i].active)) continue;
        atomic_store(&s_peers[i].close_requested, true);
        EventBits_t bits = xEventGroupWaitBits(s_peers[i].events, BIT_CLOSED, pdFALSE, pdTRUE, pdMS_TO_TICKS(5000));
        if (!(bits & BIT_CLOSED)) result = ESP_ERR_TIMEOUT;
    }
    return result;
}

bool transport_client_session_active(const uint8_t fingerprint[32], uint32_t generation)
{
    if (!fingerprint) return false;
    portENTER_CRITICAL(&s_peer_mux);
    transport_peer_t *slot = find_slot(fingerprint, generation);
    bool active = slot && !atomic_load(&slot->failed) && !atomic_load(&slot->identity_rejected) && !atomic_load(&slot->close_requested);
    portEXIT_CRITICAL(&s_peer_mux);
    return active;
}

esp_err_t transport_close_for_client(const uint8_t fingerprint[32], uint32_t generation)
{
    if (!fingerprint) return ESP_ERR_INVALID_ARG;
    portENTER_CRITICAL(&s_peer_mux);
    transport_peer_t *slot = find_slot(fingerprint, generation);
    if (slot) { atomic_store(&slot->application_authorized, false); atomic_store(&slot->close_requested, true); }
    portEXIT_CRITICAL(&s_peer_mux);
    if (!slot) return ESP_OK;
    EventBits_t bits = xEventGroupWaitBits(slot->events, BIT_CLOSED, pdFALSE, pdTRUE, pdMS_TO_TICKS(5000));
    return (bits & BIT_CLOSED) ? ESP_OK : ESP_ERR_TIMEOUT;
}

esp_err_t transport_authorize_client(const uint8_t fingerprint[32], uint32_t generation)
{
    if (!fingerprint || !client_auth_is_enrolled(fingerprint)) return ESP_ERR_INVALID_STATE;
    portENTER_CRITICAL(&s_peer_mux);
    transport_peer_t *slot = find_slot(fingerprint, generation);
    bool ready = slot && !atomic_load(&slot->failed) && !atomic_load(&slot->identity_rejected) && slot->open_channels == 2;
    if (ready) atomic_store(&slot->application_authorized, true);
    portEXIT_CRITICAL(&s_peer_mux);
    return ready && client_auth_is_enrolled(fingerprint) ? ESP_OK : ESP_ERR_INVALID_STATE;
}

void transport_invalidate_client(const uint8_t fingerprint[32])
{
    if (!fingerprint) return;
    portENTER_CRITICAL(&s_peer_mux);
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (atomic_load(&s_peers[i].active) && memcmp(s_peers[i].client_fingerprint, fingerprint, 32) == 0) {
            atomic_store(&s_peers[i].application_authorized, false);
            atomic_store(&s_peers[i].close_requested, true);
        }
    }
    portEXIT_CRITICAL(&s_peer_mux);
}

void transport_invalidate_all_clients(void)
{
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) if (atomic_load(&s_peers[i].active)) {
        atomic_store(&s_peers[i].application_authorized, false);
        atomic_store(&s_peers[i].close_requested, true);
    }
}

transport_status_t transport_status(void)
{
    bool any_active = false, any_open = false, any_failed = false, any_identity_rejected = false;
    bool inactive_failed = false, inactive_identity_rejected = false;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        if (atomic_load(&s_peers[i].active)) {
            any_active = true;
            any_open |= s_peers[i].open_channels == 2;
            any_failed |= atomic_load(&s_peers[i].failed);
            any_identity_rejected |= atomic_load(&s_peers[i].identity_rejected);
        } else {
            inactive_failed |= atomic_load(&s_peers[i].failed);
            inactive_identity_rejected |= atomic_load(&s_peers[i].identity_rejected);
        }
    }
    if (!any_active) {
        if (inactive_identity_rejected) return TRANSPORT_STATUS_IDENTITY_REJECTED;
        return inactive_failed ? TRANSPORT_STATUS_FAILED : TRANSPORT_STATUS_IDLE;
    }
    if (any_identity_rejected) return TRANSPORT_STATUS_IDENTITY_REJECTED;
    if (any_failed) return TRANSPORT_STATUS_FAILED;
    return any_open ? TRANSPORT_STATUS_OPEN : TRANSPORT_STATUS_CONNECTING;
}

void transport_get_metrics(transport_metrics_t *metrics)
{
    if (!metrics) return;
    bool monitoring = atomic_load(&s_heap_monitor);
    metrics->state = transport_status();
    metrics->internal_free_start = atomic_load(&s_internal_free_start);
    metrics->internal_free_min = monitoring ? heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL) : atomic_load(&s_internal_free_min);
    metrics->internal_free_end = monitoring ? heap_caps_get_free_size(MALLOC_CAP_INTERNAL) : atomic_load(&s_internal_free_end);
    metrics->psram_free_start = atomic_load(&s_psram_free_start);
    metrics->psram_free_min = monitoring ? heap_caps_get_minimum_free_size(MALLOC_CAP_SPIRAM) : atomic_load(&s_psram_free_min);
    metrics->psram_free_end = monitoring ? heap_caps_get_free_size(MALLOC_CAP_SPIRAM) : atomic_load(&s_psram_free_end);
    metrics->channel_opens = atomic_load(&s_channel_opens);
    metrics->application_bytes_received = atomic_load(&s_application_bytes_received);
    metrics->peer_task_cpu_us = atomic_load(&s_peer_task_cpu_us);
    metrics->tls_rate_limited = atomic_load(&s_tls_rate_limited);
    metrics->board_timestamp_ms = atomic_load(&s_last_event_timestamp_ms);
    metrics->last_event_sequence = atomic_load(&s_last_event_sequence);
    metrics->scripted_events = atomic_load(&s_scripted_events);
    metrics->scripted_rejections = atomic_load(&s_scripted_rejections);
    metrics->scripted_drops = atomic_load(&s_scripted_drops);
    metrics->scripted_reorders = atomic_load(&s_scripted_reorders);
    metrics->response_queue_depth = 0;
    for (size_t i = 0; i < MAX_TRANSPORT_PEERS; ++i) {
        portENTER_CRITICAL(&s_response_mux[i]);
        if (s_peers[i].responses.count > metrics->response_queue_depth) {
            metrics->response_queue_depth = s_peers[i].responses.count;
        }
        portEXIT_CRITICAL(&s_response_mux[i]);
    }
    portENTER_CRITICAL(&s_lease_mux);
    metrics->lease_epoch = s_lease.epoch;
    metrics->normalized_keys = s_lease.held_keys ? 1 : 0;
    metrics->normalized_buttons = s_lease.held_buttons ? 1 : 0;
    portEXIT_CRITICAL(&s_lease_mux);
    portENTER_CRITICAL(&s_tls_diagnostic_mux);
    memcpy(metrics->tls_diagnostic, s_tls_diagnostic, sizeof(metrics->tls_diagnostic));
    portEXIT_CRITICAL(&s_tls_diagnostic_mux);
}
