/* Isolated M0 backend check, based on Espressif's CC0 peer_demo.
 * No USB HID, physical input, NVS, credentials, STUN or signaling listener.
 * Two peers share an isolated SoftAP interface; no station needs to join.
 */
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_event.h"
#include "esp_heap_caps.h"
#include "esp_netif.h"
#include "esp_random.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "esp_peer_default.h"

typedef struct {
    esp_peer_handle_t handle;
    atomic_bool running, stopped, opened;
    atomic_uint received, failures;
    int index;
} peer_t;

static peer_t peers[2];
static int corrupt_source = -1;
static atomic_uint mutations, signal_errors;
static const char payload[] = "M0 fingerprint probe";

/* Change one hex digit, preserving valid SDP syntax and certificate itself. */
static unsigned corrupt_fingerprints(char *sdp)
{
    unsigned count = 0;
    char *p = sdp;
    while ((p = strstr(p, "a=fingerprint:sha-256 ")) != NULL) {
        p += strlen("a=fingerprint:sha-256 ");
        if (strchr("0123456789abcdefABCDEF", *p) == NULL || *p == '\0') {
            return 0;
        }
        *p = (*p == '0') ? '1' : '0';
        ++count;
    }
    return count;
}

static int on_message(esp_peer_msg_t *msg, void *ctx)
{
    peer_t *source = ctx;
    if (msg->type != ESP_PEER_MSG_TYPE_SDP) return 0;
    esp_peer_msg_t forwarded = *msg;
    char *copy = calloc(1, msg->size + 1);
    if (!copy) { ++signal_errors; return -1; }
    memcpy(copy, msg->data, msg->size);
    if (source->index == corrupt_source) {
        unsigned count = corrupt_fingerprints(copy);
        mutations += count;
        if (!count) { ++signal_errors; free(copy); return -1; }
        printf("M0 MUTATED source=%d fingerprints=%u\n", source->index, count);
    }
    /* Log only role and fingerprint lines; no ICE credentials. */
    const char *role = strstr(copy, "a=setup:");
    printf("M0 SDP source=%d setup=%.*s bytes=%d\n", source->index,
           role ? (int)strcspn(role, "\r\n") : 0, role ? role : "", msg->size);
    for (const char *line = copy; *line; ) {
        size_t length = strcspn(line, "\r\n");
        if (!strncmp(line, "m=", 2) || !strncmp(line, "a=mid:", 6) ||
            !strncmp(line, "a=sctp", 6) || !strncmp(line, "a=inactive", 10)) {
            printf("M0 SDP_LINE source=%d %.*s\n", source->index, (int)length, line);
        }
        line += length;
        line += strspn(line, "\r\n");
    }
    forwarded.data = (uint8_t *)copy;
    int ret = esp_peer_send_msg(peers[1 - source->index].handle, &forwarded);
    if (ret) ++signal_errors;
    free(copy);
    return ret;
}

static int on_state(esp_peer_state_t state, void *ctx)
{
    peer_t *peer = ctx;
    printf("M0 STATE peer=%d state=%d\n", peer->index, state);
    if (state == ESP_PEER_STATE_DATA_CHANNEL_OPENED ||
        state == ESP_PEER_STATE_DATA_CHANNEL_CONNECTED) peer->opened = true;
    if (state == ESP_PEER_STATE_CONNECT_FAILED) ++peer->failures;
    return 0;
}

static int on_channel(esp_peer_data_channel_info_t *channel, void *ctx)
{
    (void)channel;
    ((peer_t *)ctx)->opened = true;
    return 0;
}

static int on_data(esp_peer_data_frame_t *frame, void *ctx)
{
    peer_t *peer = ctx;
    if (frame->size == sizeof(payload) && !memcmp(frame->data, payload, sizeof(payload))) {
        ++peer->received;
    } else {
        ++signal_errors;
    }
    return 0;
}

static void peer_task(void *arg)
{
    peer_t *peer = arg;
    unsigned sent = 0;
    while (peer->running) {
        esp_peer_main_loop(peer->handle);
        if (peer->opened && sent < 10) {
            esp_peer_data_frame_t frame = {
                .type = ESP_PEER_DATA_CHANNEL_DATA,
                .data = (uint8_t *)payload, .size = sizeof(payload),
            };
            if (!esp_peer_send_data(peer->handle, &frame)) ++sent;
        }
        vTaskDelay(pdMS_TO_TICKS(20));
    }
    peer->stopped = true;
    vTaskDelete(NULL);
}

static void log_heap(const char *phase)
{
    printf("M0 HEAP phase=%s internal=%u minimum=%u psram=%u\n", phase,
           (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
           (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
           (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
}

static bool run_case(int source)
{
    corrupt_source = source;
    mutations = signal_errors = 0;
    memset(peers, 0, sizeof(peers));
    printf("M0 CASE source=%d begin\n", source);
    esp_peer_default_cfg_t defaults = {
        /* Backend retries 11 reads; allow DTLS's 1s initial retransmit. */
        .agent_recv_timeout = 200,
        .data_ch_cfg = {.send_cache_size = 4096, .recv_cache_size = 4096},
        .keep_role = true,
    };
    for (int i = 0; i < 2; ++i) {
        peers[i].index = i;
        esp_peer_cfg_t config = {
            .role = i == 0 ? ESP_PEER_ROLE_CONTROLLING : ESP_PEER_ROLE_CONTROLLED,
            .audio_dir = ESP_PEER_MEDIA_DIR_NONE,
            .video_dir = ESP_PEER_MEDIA_DIR_NONE,
            .enable_data_channel = true,
            .no_auto_reconnect = true,
            .on_state = on_state, .on_msg = on_message,
            .on_data = on_data, .on_channel_open = on_channel,
            .ctx = &peers[i], .extra_cfg = &defaults, .extra_size = sizeof(defaults),
        };
        ESP_ERROR_CHECK(esp_peer_open(&config, esp_peer_get_default_impl(), &peers[i].handle));
    }
    /* Prepare both sessions before workers can forward SDP or verify pins. */
    ESP_ERROR_CHECK(esp_peer_new_connection(peers[0].handle));
    ESP_ERROR_CHECK(esp_peer_new_connection(peers[1].handle));
    for (int i = 0; i < 2; ++i) {
        peers[i].running = true;
        configASSERT(xTaskCreatePinnedToCore(peer_task, "m0_peer", 12 * 1024,
                                            &peers[i], 5, NULL, i) == pdPASS);
    }
    log_heap("peers-started");
    int64_t deadline = esp_timer_get_time() + 20000000;
    while (esp_timer_get_time() < deadline) vTaskDelay(pdMS_TO_TICKS(100));
    log_heap("case-end");
    for (int i = 0; i < 2; ++i) peers[i].running = false;
    for (int i = 0; i < 2; ++i) {
        while (!peers[i].stopped) vTaskDelay(pdMS_TO_TICKS(20));
    }
    bool opened = peers[0].opened || peers[1].opened;
    bool exchanged = peers[0].received > 0 && peers[1].received > 0;
    bool unexpected = opened || peers[0].received || peers[1].received;
    /* A generic connection failure cannot prove fingerprint enforcement. */
    bool pass = source < 0 && !signal_errors && exchanged;
    printf("M0 RESULT source=%d result=%s mutations=%u errors=%u opened=%d,%d received=%u,%u failures=%u,%u\n",
           source, pass ? "PASS" : (source >= 0 && unexpected && mutations && !signal_errors ? "SECURITY_FAIL" : "INCONCLUSIVE"),
           (unsigned)mutations, (unsigned)signal_errors,
           (int)peers[0].opened, (int)peers[1].opened,
           (unsigned)peers[0].received, (unsigned)peers[1].received,
           (unsigned)peers[0].failures, (unsigned)peers[1].failures);
    for (int i = 0; i < 2; ++i) ESP_ERROR_CHECK(esp_peer_close(peers[i].handle));
    log_heap("peers-closed");
    fflush(stdout);
    return pass;
}

static void probe_task(void *arg)
{
    (void)arg;
    char test[] = "a=fingerprint:sha-256 AB:CD\r\na=fingerprint:sha-256 00:FF\r\n";
    configASSERT(corrupt_fingerprints(test) == 2);
    configASSERT(!strcmp(test, "a=fingerprint:sha-256 0B:CD\r\na=fingerprint:sha-256 10:FF\r\n"));
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    configASSERT(esp_netif_create_default_wifi_ap());
    wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&init));
    ESP_ERROR_CHECK(esp_wifi_set_storage(WIFI_STORAGE_RAM));
    wifi_config_t wifi = {.ap = {.ssid = "kvmctl-m0-isolated", .channel = 6,
                                .max_connection = 1, .authmode = WIFI_AUTH_WPA2_PSK}};
    for (int i = 0; i < 32; ++i) wifi.ap.password[i] = "0123456789abcdef"[esp_random() & 15];
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_AP));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_AP, &wifi));
    memset(&wifi, 0, sizeof(wifi));
    ESP_ERROR_CHECK(esp_wifi_start());
    ESP_ERROR_CHECK(esp_peer_pre_generate_cert());
    log_heap("ready");
    if (!run_case(-1)) {
        printf("M0 SUITE INCONCLUSIVE baseline_failed\n");
        vTaskDelete(NULL);
    }
    run_case(0);
    run_case(1);
    /* Host combines counters with explicit DTLS mismatch reasons and roles. */
    printf("M0 SUITE COMPLETE\n");
    vTaskDelete(NULL);
}

void app_main(void)
{
    /* Certificate generation exceeds IDF's small default main-task stack. */
    configASSERT(xTaskCreate(probe_task, "m0_suite", 24 * 1024, NULL, 4, NULL) == pdPASS);
}
