#include "transport.h"

#include <stdbool.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdatomic.h>
#include <string.h>
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_peer_default.h"
#include "wifi_setup.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"

#define SDP_CAPACITY 8192
#define CONTROL_CAPACITY 512
#define MOTION_CAPACITY 64
#define BIT_ANSWER BIT0
#define BIT_FAILED BIT1
#define BIT_CLOSED BIT2
#define BIT_CERT_READY BIT3

typedef struct {
    uint16_t stream;
    bool open;
    bool pending;
    uint8_t data[CONTROL_CAPACITY];
    size_t size;
    esp_peer_data_frame_t echo;
} echo_channel_t;

static EventGroupHandle_t s_events;
static atomic_bool s_active;
static atomic_bool s_close_requested;
static atomic_bool s_failed;
static atomic_bool s_identity_rejected;
static atomic_bool s_heap_monitor;
static atomic_uint s_open_channels;
static atomic_uint s_channel_opens;
static atomic_uint s_application_bytes_received;
static atomic_uint s_peer_task_cpu_us;
static atomic_uint s_internal_free_start;
static atomic_uint s_internal_free_min;
static atomic_uint s_internal_free_end;
static atomic_uint s_psram_free_start;
static atomic_uint s_psram_free_min;
static atomic_uint s_psram_free_end;
static char s_offer[SDP_CAPACITY + 1];
static uint8_t s_answer[SDP_CAPACITY + 1];
static size_t s_answer_len;
static echo_channel_t s_control;
static echo_channel_t s_motion;

static void stop_heap_monitor(void)
{
    if (!atomic_load(&s_heap_monitor)) return;
    atomic_store(&s_internal_free_min, heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL));
    atomic_store(&s_internal_free_end, heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
    atomic_store(&s_psram_free_min, heap_caps_get_minimum_free_size(MALLOC_CAP_SPIRAM));
    atomic_store(&s_psram_free_end, heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
    heap_caps_monitor_local_minimum_free_size_stop();
    atomic_store(&s_heap_monitor, false);
}

static void finish_closed_peer(void)
{
    if (atomic_load(&s_heap_monitor)) {
        vTaskDelay(1);
        stop_heap_monitor();
    }
}

static int capture_peer_log(const char *format, va_list args)
{
    char message[256];
    va_list copy;
    va_copy(copy, args);
    int length = vsnprintf(message, sizeof(message), format, copy);
    va_end(copy);
    if (length > 0 && strstr(message, "PEER_FP_REJECT")) {
        atomic_store(&s_identity_rejected, true);
    }
    return 0;
}

void transport_silence_logs(void)
{
    esp_log_set_vprintf(capture_peer_log);
}

static void fail_session(void)
{
    atomic_store(&s_failed, true);
    xEventGroupSetBits(s_events, BIT_FAILED);
}

static int on_state(esp_peer_state_t state, void *ctx)
{
    (void)ctx;
    if (!atomic_load(&s_close_requested) &&
        (state == ESP_PEER_STATE_CONNECT_FAILED ||
         state == ESP_PEER_STATE_DATA_CHANNEL_DISCONNECTED)) {
        fail_session();
    }
    return 0;
}

static int on_message(esp_peer_msg_t *message, void *ctx)
{
    (void)ctx;
    if (!message || message->type != ESP_PEER_MSG_TYPE_SDP || !message->data || message->size <= 0 ||
        message->size > SDP_CAPACITY || s_answer_len != 0) {
        fail_session();
        return -1;
    }
    memcpy(s_answer, message->data, (size_t)message->size);
    s_answer_len = (size_t)message->size;
    xEventGroupSetBits(s_events, BIT_ANSWER);
    return 0;
}

static int on_channel(esp_peer_data_channel_info_t *channel, void *ctx)
{
    (void)ctx;
    if (!channel || !channel->label) {
        fail_session();
        return -1;
    }
    echo_channel_t *target = NULL;
    if (strcmp(channel->label, "control") == 0) target = &s_control;
    if (strcmp(channel->label, "motion") == 0) target = &s_motion;
    if (!target || target->open ||
        (s_control.open && channel->stream_id == s_control.stream) ||
        (s_motion.open && channel->stream_id == s_motion.stream)) {
        fail_session();
        return -1;
    }
    target->stream = channel->stream_id;
    target->open = true;
    atomic_fetch_add(&s_open_channels, 1);
    atomic_fetch_add(&s_channel_opens, 1);
    return 0;
}

static int on_channel_close(esp_peer_data_channel_info_t *channel, void *ctx)
{
    (void)channel;
    (void)ctx;
    if (!atomic_load(&s_close_requested)) fail_session();
    return 0;
}

static int on_data(esp_peer_data_frame_t *frame, void *ctx)
{
    (void)ctx;
    if (!frame || !frame->data) {
        fail_session();
        return -1;
    }
    echo_channel_t *target = NULL;
    size_t capacity = 0;
    if (s_control.open && frame->stream_id == s_control.stream) {
        target = &s_control;
        capacity = CONTROL_CAPACITY;
    } else if (s_motion.open && frame->stream_id == s_motion.stream) {
        target = &s_motion;
        capacity = MOTION_CAPACITY;
    }
    if (!target || target->pending || frame->size <= 0 ||
        (size_t)frame->size > capacity) {
        fail_session();
        return -1;
    }
    memcpy(target->data, frame->data, (size_t)frame->size);
    atomic_fetch_add(&s_application_bytes_received, (uint32_t)frame->size);
    target->size = (size_t)frame->size;
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

static void peer_task(void *arg)
{
    (void)arg;
    TaskHandle_t task = xTaskGetCurrentTaskHandle();
    TaskStatus_t task_info;
    vTaskGetInfo(task, &task_info, pdFALSE, eInvalid);
    uint32_t runtime_start = (uint32_t)task_info.ulRunTimeCounter;
    esp_peer_handle_t peer = NULL;
    esp_peer_default_cfg_t defaults = {
        .agent_recv_timeout = 200,
        .keep_role = true,
        .data_ch_cfg = {.send_cache_size = 4096, .recv_cache_size = 4096},
    };
    esp_peer_cfg_t config = {
        .role = ESP_PEER_ROLE_CONTROLLED,
        .audio_dir = ESP_PEER_MEDIA_DIR_NONE,
        .video_dir = ESP_PEER_MEDIA_DIR_NONE,
        .enable_data_channel = true,
        .manual_ch_create = true,
        .no_auto_reconnect = true,
        .on_state = on_state,
        .on_msg = on_message,
        .on_data = on_data,
        .on_channel_open = on_channel,
        .on_channel_close = on_channel_close,
        .ctx = NULL,
        .extra_cfg = &defaults,
        .extra_size = sizeof(defaults),
    };
    int ret = esp_peer_open(&config, esp_peer_get_default_impl(), &peer);
    if (ret == ESP_PEER_ERR_NONE) ret = esp_peer_new_connection(peer);
    if (ret == ESP_PEER_ERR_NONE) {
        esp_peer_msg_t offer = {
            .type = ESP_PEER_MSG_TYPE_SDP,
            .data = (uint8_t *)s_offer,
            .size = (int)strlen(s_offer),
        };
        ret = esp_peer_send_msg(peer, &offer);
        if (ret == ESP_PEER_ERR_INVALID_ARG) atomic_store(&s_identity_rejected, true);
    }
    if (ret != ESP_PEER_ERR_NONE) {
        atomic_store(&s_failed, true);
        xEventGroupSetBits(s_events, BIT_FAILED);
    } else {
        while (!atomic_load(&s_close_requested) && !atomic_load(&s_failed)) {
            esp_peer_main_loop(peer);
            if (!send_echo(peer, &s_control) || !send_echo(peer, &s_motion)) {
                fail_session();
            }
            vTaskDelay(pdMS_TO_TICKS(10));
        }
    }

    /* Close only after main_loop and all callbacks have returned. */
    if (peer) esp_peer_close(peer);
    memset(s_offer, 0, sizeof(s_offer));
    memset(s_answer, 0, sizeof(s_answer));
    memset(&s_control, 0, sizeof(s_control));
    memset(&s_motion, 0, sizeof(s_motion));
    s_answer_len = 0;
    atomic_store(&s_open_channels, 0);
    vTaskDelay(1);
    vTaskGetInfo(task, &task_info, pdFALSE, eInvalid);
    atomic_store(&s_peer_task_cpu_us,
                 (uint32_t)task_info.ulRunTimeCounter - runtime_start);
    atomic_store(&s_active, false);
    xEventGroupSetBits(s_events, BIT_CLOSED);
    vTaskDelete(NULL);
}

static void certificate_task(void *arg)
{
    (void)arg;
    int result = esp_peer_pre_generate_cert();
    xEventGroupSetBits(s_events, result == ESP_PEER_ERR_NONE ? BIT_CERT_READY : BIT_FAILED);
    vTaskDelete(NULL);
}

esp_err_t transport_init(void)
{
    s_events = xEventGroupCreate();
    if (!s_events) return ESP_ERR_NO_MEM;
    if (xTaskCreate(certificate_task, "peer_cert", 24 * 1024, NULL, 4, NULL) != pdPASS) {
        vEventGroupDelete(s_events);
        s_events = NULL;
        return ESP_ERR_NO_MEM;
    }
    EventBits_t bits = xEventGroupWaitBits(s_events, BIT_CERT_READY | BIT_FAILED,
                                           pdFALSE, pdFALSE, pdMS_TO_TICKS(30000));
    return (bits & BIT_CERT_READY) ? ESP_OK : ESP_FAIL;
}

esp_err_t transport_offer(const uint8_t *offer, size_t offer_len,
                          uint8_t *answer, size_t answer_capacity, size_t *answer_len)
{
    if (!s_events || !offer || !answer || !answer_len || offer_len == 0 ||
        offer_len > SDP_CAPACITY || offer_len < 5 || memcmp(offer, "v=0\r\n", 5) != 0 ||
        memchr(offer, 0, offer_len)) return ESP_ERR_INVALID_ARG;
    if (wifi_setup_state() != 2) return ESP_ERR_INVALID_STATE;
    for (size_t i = 0; i < offer_len; ++i) {
        if ((offer[i] < 0x20 && offer[i] != '\r' && offer[i] != '\n' && offer[i] != '\t') ||
            offer[i] > 0x7e) return ESP_ERR_INVALID_ARG;
    }
    if (atomic_load(&s_active)) {
        /* Second SDP invalidates the session. Worker closes after callbacks unwind. */
        atomic_store(&s_close_requested, true);
        EventBits_t bits = xEventGroupWaitBits(s_events, BIT_CLOSED, pdFALSE, pdTRUE,
                                               pdMS_TO_TICKS(5000));
        if (bits & BIT_CLOSED) finish_closed_peer();
        return ESP_ERR_INVALID_STATE;
    }
    if (heap_caps_monitor_local_minimum_free_size_start() != ESP_OK) {
        return ESP_ERR_INVALID_STATE;
    }
    atomic_store(&s_heap_monitor, true);
    atomic_store(&s_internal_free_start, heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
    atomic_store(&s_internal_free_min, atomic_load(&s_internal_free_start));
    atomic_store(&s_internal_free_end, 0);
    atomic_store(&s_psram_free_start, heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
    atomic_store(&s_psram_free_min, atomic_load(&s_psram_free_start));
    atomic_store(&s_psram_free_end, 0);
    xEventGroupClearBits(s_events, BIT_ANSWER | BIT_FAILED | BIT_CLOSED);
    atomic_store(&s_failed, false);
    atomic_store(&s_identity_rejected, false);
    atomic_store(&s_open_channels, 0);
    atomic_store(&s_channel_opens, 0);
    atomic_store(&s_application_bytes_received, 0);
    atomic_store(&s_peer_task_cpu_us, 0);
    atomic_store(&s_close_requested, false);
    atomic_store(&s_active, true);
    memcpy(s_offer, offer, offer_len);
    s_offer[offer_len] = 0;
    if (xTaskCreate(peer_task, "webrtc_peer", 24 * 1024, NULL, 5, NULL) != pdPASS) {
        atomic_store(&s_active, false);
        memset(s_offer, 0, sizeof(s_offer));
        stop_heap_monitor();
        return ESP_ERR_NO_MEM;
    }
    EventBits_t bits = xEventGroupWaitBits(s_events, BIT_ANSWER | BIT_FAILED,
                                           pdFALSE, pdFALSE, pdMS_TO_TICKS(30000));
    if (!(bits & BIT_ANSWER) || atomic_load(&s_failed)) {
        atomic_store(&s_close_requested, true);
        bits = xEventGroupWaitBits(s_events, BIT_CLOSED, pdFALSE, pdTRUE,
                                   pdMS_TO_TICKS(5000));
        if (bits & BIT_CLOSED) finish_closed_peer();
        if (atomic_load(&s_identity_rejected)) return ESP_ERR_INVALID_ARG;
        return ESP_ERR_INVALID_RESPONSE;
    }
    if (s_answer_len > answer_capacity) {
        atomic_store(&s_close_requested, true);
        bits = xEventGroupWaitBits(s_events, BIT_CLOSED, pdFALSE, pdTRUE,
                                   pdMS_TO_TICKS(5000));
        if (bits & BIT_CLOSED) finish_closed_peer();
        return ESP_ERR_INVALID_SIZE;
    }
    memcpy(answer, s_answer, s_answer_len);
    *answer_len = s_answer_len;
    return ESP_OK;
}

esp_err_t transport_close(void)
{
    if (!s_events || !atomic_load(&s_active)) return ESP_OK;
    atomic_store(&s_close_requested, true);
    EventBits_t bits = xEventGroupWaitBits(s_events, BIT_CLOSED, pdFALSE, pdTRUE,
                                           pdMS_TO_TICKS(5000));
    if (bits & BIT_CLOSED) finish_closed_peer();
    return (bits & BIT_CLOSED) ? ESP_OK : ESP_ERR_TIMEOUT;
}

transport_status_t transport_status(void)
{
    if (atomic_load(&s_identity_rejected)) return TRANSPORT_STATUS_IDENTITY_REJECTED;
    if (atomic_load(&s_failed)) return TRANSPORT_STATUS_FAILED;
    if (!atomic_load(&s_active)) return TRANSPORT_STATUS_IDLE;
    return atomic_load(&s_open_channels) == 2 ? TRANSPORT_STATUS_OPEN : TRANSPORT_STATUS_CONNECTING;
}

void transport_get_metrics(transport_metrics_t *metrics)
{
    if (!metrics) return;
    bool monitoring = atomic_load(&s_heap_monitor);
    metrics->state = transport_status();
    metrics->internal_free_start = atomic_load(&s_internal_free_start);
    metrics->internal_free_min = monitoring
        ? heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL)
        : atomic_load(&s_internal_free_min);
    metrics->internal_free_end = monitoring
        ? heap_caps_get_free_size(MALLOC_CAP_INTERNAL)
        : atomic_load(&s_internal_free_end);
    metrics->psram_free_start = atomic_load(&s_psram_free_start);
    metrics->psram_free_min = monitoring
        ? heap_caps_get_minimum_free_size(MALLOC_CAP_SPIRAM)
        : atomic_load(&s_psram_free_min);
    metrics->psram_free_end = monitoring
        ? heap_caps_get_free_size(MALLOC_CAP_SPIRAM)
        : atomic_load(&s_psram_free_end);
    metrics->channel_opens = atomic_load(&s_channel_opens);
    metrics->application_bytes_received = atomic_load(&s_application_bytes_received);
    metrics->peer_task_cpu_us = atomic_load(&s_peer_task_cpu_us);
}
