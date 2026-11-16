/* M0 ESP32/Rust DataChannel bench. Signaling stays on direct UART.
 * One Rust offer, one board answer; no STUN or network signaling listener.
 */
#include "serial_peer.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_heap_caps.h"
#include "esp_peer_default.h"

#define SDP_CAPACITY 8192
#define LINE_CAPACITY (2 * SDP_CAPACITY + 32)
#define DATA_CAPACITY 512

typedef struct {
    esp_peer_handle_t handle;
    atomic_bool running, stopped;
    bool connected, channel_open, pending;
    uint16_t stream;
    unsigned received, echoed;
    uint8_t data[DATA_CAPACITY];
    esp_peer_data_frame_t echo;
} serial_peer_t;

static void heap_marker(const char *phase)
{
    printf("M0 PEER HEAP phase=%s internal=%u minimum=%u psram=%u\n", phase,
           (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
           (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
           (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
    fflush(stdout);
}

static int hex_digit(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static int decode_sdp(const char *hex, uint8_t *sdp)
{
    size_t length = strlen(hex);
    if (!length || (length & 1) || length > 2 * SDP_CAPACITY) return -1;
    for (size_t i = 0; i < length / 2; ++i) {
        int hi = hex_digit(hex[2 * i]), lo = hex_digit(hex[2 * i + 1]);
        if (hi < 0 || lo < 0 || (!hi && !lo)) return -1;
        sdp[i] = (uint8_t)((hi << 4) | lo);
    }
    sdp[length / 2] = 0;
    return (int)(length / 2);
}

static int on_message(esp_peer_msg_t *message, void *ctx)
{
    (void)ctx;
    if (message->type != ESP_PEER_MSG_TYPE_SDP) return 0;
    int size = message->size;
    if (size > 0 && message->data[size - 1] == 0) --size;
    if (size <= 0 || size > SDP_CAPACITY) {
        printf("M0 PEER ERROR sdp_size\n");
        return -1;
    }
    char *hex = malloc(2 * size + 1);
    if (!hex) return -1;
    static const char digits[] = "0123456789abcdef";
    for (int i = 0; i < size; ++i) {
        hex[2 * i] = digits[message->data[i] >> 4];
        hex[2 * i + 1] = digits[message->data[i] & 15];
    }
    hex[2 * size] = 0;
    /* One stdio write prevents other task logs splitting this UART record.
     * Host bridge must consume this record without persisting ICE secrets.
     */
    printf("M0 PEER SDP %s\n", hex);
    fflush(stdout);
    memset(hex, 0, 2 * size);
    free(hex);
    return 0;
}

static int on_state(esp_peer_state_t state, void *ctx)
{
    serial_peer_t *peer = ctx;
    printf("M0 PEER STATE %d\n", state);
    if (!peer->connected && (state == ESP_PEER_STATE_DATA_CHANNEL_CONNECTED ||
                             state == ESP_PEER_STATE_DATA_CHANNEL_OPENED)) {
        peer->connected = true;
        printf("M0 PEER CONNECTED\n");
        heap_marker("connected");
    }
    fflush(stdout);
    return 0;
}

static int on_channel(esp_peer_data_channel_info_t *channel, void *ctx)
{
    serial_peer_t *peer = ctx;
    if (!channel->label || strcmp(channel->label, "control")) return -1;
    peer->stream = channel->stream_id;
    peer->channel_open = true;
    printf("M0 PEER CHANNEL control\n");
    fflush(stdout);
    return 0;
}

static int on_data(esp_peer_data_frame_t *frame, void *ctx)
{
    serial_peer_t *peer = ctx;
    if (!peer->channel_open || frame->stream_id != peer->stream ||
        frame->size <= 0 || frame->size > DATA_CAPACITY || peer->pending) {
        printf("M0 PEER ERROR data_bounds_or_busy\n");
        fflush(stdout);
        return -1;
    }
    /* ponytail: one pending echo; runner uses stop-and-wait, not pipelining. */
    memcpy(peer->data, frame->data, frame->size);
    peer->echo = *frame;
    peer->echo.data = peer->data;
    peer->pending = true;
    ++peer->received;
    return 0;
}

static void peer_task(void *arg)
{
    serial_peer_t *peer = arg;
    while (peer->running) {
        esp_peer_main_loop(peer->handle);
        if (peer->pending) {
            int ret = esp_peer_send_data(peer->handle, &peer->echo);
            if (ret != ESP_PEER_ERR_WOULD_BLOCK) {
                peer->pending = false;
                if (!ret) ++peer->echoed;
                else printf("M0 PEER ERROR echo=%d\n", ret);
            }
        }
        vTaskDelay(pdMS_TO_TICKS(10));
    }
    peer->stopped = true;
    vTaskDelete(NULL);
}

/* Preserve partial reads when the polling console VFS returns EOF between
 * UART chunks. Oversized input is drained through its newline before reuse.
 */
static bool read_command(char *line)
{
    size_t used = 0;
    bool overflow = false;
    for (;;) {
        int c = getchar();
        if (c == EOF) {
            clearerr(stdin);
            vTaskDelay(pdMS_TO_TICKS(1));
            continue;
        }
        if (c == '\r') continue;
        if (c == '\n') {
            line[used] = 0;
            return !overflow;
        }
        if (c == 0 || used == LINE_CAPACITY - 1) overflow = true;
        if (!overflow) line[used++] = (char)c;
    }
}

void run_serial_peer(void)
{
    uint8_t check[16];
    configASSERT(decode_sdp("763d300d0a", check) == 5);
    configASSERT(!strcmp((char *)check, "v=0\r\n"));
    configASSERT(decode_sdp("+1", check) < 0);
    configASSERT(decode_sdp("00", check) < 0);
    configASSERT(decode_sdp("f", check) < 0);
    char *line = malloc(LINE_CAPACITY);
    uint8_t *sdp = malloc(SDP_CAPACITY + 1);
    if (!line || !sdp) {
        free(line);
        free(sdp);
        printf("M0 PEER ERROR memory\n");
        return;
    }
    ESP_ERROR_CHECK(esp_peer_pre_generate_cert());
    serial_peer_t peer = {0};
    heap_marker("serial-ready");
    printf("M0 PEER READY\n");
    fflush(stdout);
    for (;;) {
        if (!read_command(line)) {
            printf("M0 PEER ERROR line_too_long\n");
        } else if (!strcmp(line, "PEER STOP")) {
            if (peer.handle) {
                peer.running = false;
                while (!peer.stopped) vTaskDelay(pdMS_TO_TICKS(10));
                esp_peer_close(peer.handle);
                printf("M0 PEER COUNTS received=%u echoed=%u\n", peer.received, peer.echoed);
                memset(&peer, 0, sizeof(peer));
                heap_marker("closed");
            }
            printf("M0 PEER STOPPED\n");
        } else if (!strncmp(line, "PEER OFFER ", 11) && !peer.handle) {
            int size = decode_sdp(line + 11, sdp);
            if (size < 0) {
                printf("M0 PEER ERROR invalid_sdp_hex\n");
            } else {
                esp_peer_default_cfg_t defaults = {
                    .agent_recv_timeout = 200, .keep_role = true,
                    .data_ch_cfg = {.send_cache_size = 4096, .recv_cache_size = 4096},
                };
                esp_peer_cfg_t config = {
                    .role = ESP_PEER_ROLE_CONTROLLED,
                    .audio_dir = ESP_PEER_MEDIA_DIR_NONE,
                    .video_dir = ESP_PEER_MEDIA_DIR_NONE,
                    .enable_data_channel = true, .manual_ch_create = true,
                    .no_auto_reconnect = true,
                    .on_state = on_state, .on_msg = on_message,
                    .on_data = on_data, .on_channel_open = on_channel,
                    .ctx = &peer, .extra_cfg = &defaults, .extra_size = sizeof(defaults),
                };
                int ret = esp_peer_open(&config, esp_peer_get_default_impl(), &peer.handle);
                if (!ret) ret = esp_peer_new_connection(peer.handle);
                esp_peer_msg_t offer = {.type = ESP_PEER_MSG_TYPE_SDP, .data = sdp, .size = size};
                if (!ret) ret = esp_peer_send_msg(peer.handle, &offer);
                if (!ret) {
                    peer.running = true;
                    if (xTaskCreate(peer_task, "m0_serial_peer", 16 * 1024, &peer, 5, NULL) != pdPASS) {
                        peer.running = false;
                        ret = ESP_PEER_ERR_NO_MEM;
                    }
                }
                if (ret) {
                    if (peer.handle) esp_peer_close(peer.handle);
                    memset(&peer, 0, sizeof(peer));
                    printf("M0 PEER ERROR setup=%d\n", ret);
                } else heap_marker("started");
            }
        } else {
            printf("M0 PEER ERROR command_or_busy\n");
        }
        memset(sdp, 0, SDP_CAPACITY + 1);
        memset(line, 0, LINE_CAPACITY);
        fflush(stdout);
    }
}
