#pragma once

#include "esp_err.h"
#include <stdint.h>

esp_err_t signaling_init(void);
void signaling_invalidate_client(const uint8_t fingerprint[32]);
void signaling_invalidate_all_clients(void);
