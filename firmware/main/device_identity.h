#pragma once

#include "esp_err.h"
#include <stddef.h>
#include <stdint.h>

#define DEVICE_IDENTITY_SPKI_MAX 128
#define DEVICE_IDENTITY_TLS_NAME_MAX 64

esp_err_t device_identity_init(void);
esp_err_t device_identity_status(uint8_t fingerprint[32], uint8_t tls_spki_hash[32],
                                 uint8_t spki_der[DEVICE_IDENTITY_SPKI_MAX], size_t *spki_len);
esp_err_t device_identity_tls_material(const uint8_t **certificate_pem, size_t *certificate_len,
                                       uint32_t *private_key_id,
                                       char hostname[DEVICE_IDENTITY_TLS_NAME_MAX]);
esp_err_t device_identity_sign(const uint8_t *message, size_t message_len,
                               uint8_t *signature_der, size_t signature_capacity,
                               size_t *signature_len);
