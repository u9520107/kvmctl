#pragma once

#include "esp_err.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define TRANSPORT_DIAGNOSTIC_CAPACITY 96

typedef enum {
    TRANSPORT_STATUS_IDLE = 0,
    TRANSPORT_STATUS_CONNECTING = 1,
    TRANSPORT_STATUS_OPEN = 2,
    TRANSPORT_STATUS_IDENTITY_REJECTED = 3,
    TRANSPORT_STATUS_FAILED = 4,
} transport_status_t;

typedef struct {
    transport_status_t state;
    uint32_t internal_free_start;
    uint32_t internal_free_min;
    uint32_t internal_free_end;
    uint32_t psram_free_start;
    uint32_t psram_free_min;
    uint32_t psram_free_end;
    uint32_t channel_opens;
    uint32_t application_bytes_received;
    uint32_t peer_task_cpu_us;
    uint32_t tls_rate_limited;
    uint64_t board_timestamp_ms;
    uint32_t lease_epoch;
    uint64_t last_event_sequence;
    uint32_t response_queue_depth;
    uint32_t scripted_events;
    uint32_t scripted_rejections;
    uint32_t scripted_drops;
    uint32_t scripted_reorders;
    uint32_t normalized_keys;
    uint32_t normalized_buttons;
    char tls_diagnostic[TRANSPORT_DIAGNOSTIC_CAPACITY];
} transport_metrics_t;

esp_err_t transport_init(void);
esp_err_t transport_offer(const uint8_t *offer, size_t offer_len,
                          uint8_t *answer, size_t answer_capacity, size_t *answer_len);
esp_err_t transport_offer_for_client(const uint8_t fingerprint[32],
                                     const uint8_t session_id[16],
                                     const uint8_t *offer, size_t offer_len,
                                     uint8_t *answer, size_t answer_capacity, size_t *answer_len,
                                     uint32_t *generation);
esp_err_t transport_close(void);
esp_err_t transport_close_for_client(const uint8_t fingerprint[32], uint32_t generation);
esp_err_t transport_authorize_client(const uint8_t fingerprint[32], uint32_t generation);
bool transport_client_session_active(const uint8_t fingerprint[32], uint32_t generation);
void transport_invalidate_client(const uint8_t fingerprint[32]);
void transport_invalidate_all_clients(void);
transport_status_t transport_status(void);
void transport_get_metrics(transport_metrics_t *metrics);
void transport_silence_logs(void);
void transport_note_tls_rate_limited(void);
void transport_clear_tls_diagnostic(void);
