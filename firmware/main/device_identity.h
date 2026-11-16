#pragma once

#include "esp_err.h"
#include <stddef.h>
#include <stdint.h>

#define DEVICE_IDENTITY_SPKI_MAX 128

esp_err_t device_identity_init(void);
esp_err_t device_identity_status(uint8_t fingerprint[32], uint8_t tls_spki_hash[32],
                                 uint8_t spki_der[DEVICE_IDENTITY_SPKI_MAX], size_t *spki_len);
