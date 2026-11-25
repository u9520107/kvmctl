#pragma once

#include "esp_err.h"
#include <stddef.h>
#include <stdint.h>

esp_err_t wifi_setup_init(void);
esp_err_t wifi_setup_provision(const uint8_t *ssid, size_t ssid_len,
                               const uint8_t *passphrase, size_t passphrase_len);
uint8_t wifi_setup_state(void);

typedef struct {
    uint8_t rssi_offset;
    uint8_t channel;
    uint32_t reconnect_count;
    uint32_t disconnect_count;
    uint32_t last_disconnect_reason;
    uint8_t ping_target[4];
    uint32_t ping_sent;
    uint32_t ping_received;
    uint32_t ping_min_ms;
    uint32_t ping_avg_ms;
    uint32_t ping_max_ms;
} wifi_quality_metrics_t;

esp_err_t wifi_setup_quality(wifi_quality_metrics_t *metrics);
