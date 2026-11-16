#pragma once

#include "esp_err.h"
#include <stddef.h>
#include <stdint.h>

esp_err_t wifi_setup_init(void);
esp_err_t wifi_setup_provision(const uint8_t *ssid, size_t ssid_len,
                               const uint8_t *passphrase, size_t passphrase_len);
uint8_t wifi_setup_state(void);
