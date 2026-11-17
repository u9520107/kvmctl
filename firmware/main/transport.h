#pragma once

#include "esp_err.h"
#include <stddef.h>
#include <stdint.h>

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
} transport_metrics_t;

esp_err_t transport_init(void);
esp_err_t transport_offer(const uint8_t *offer, size_t offer_len,
                          uint8_t *answer, size_t answer_capacity, size_t *answer_len);
esp_err_t transport_close(void);
transport_status_t transport_status(void);
void transport_get_metrics(transport_metrics_t *metrics);
void transport_silence_logs(void);
