#pragma once

#include "esp_err.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define CLIENT_AUTH_MAX_CLIENTS 8
#define CLIENT_AUTH_FINGERPRINT_BYTES 32
#define CLIENT_AUTH_SPKI_BYTES 91

esp_err_t client_auth_init(void);
esp_err_t client_auth_enroll(const uint8_t *spki, size_t spki_len,
                             uint8_t fingerprint[CLIENT_AUTH_FINGERPRINT_BYTES]);
esp_err_t client_auth_list(uint8_t fingerprints[CLIENT_AUTH_MAX_CLIENTS]
                                                    [CLIENT_AUTH_FINGERPRINT_BYTES],
                           size_t *count);
bool client_auth_is_enrolled(const uint8_t fingerprint[CLIENT_AUTH_FINGERPRINT_BYTES]);
esp_err_t client_auth_verify(const uint8_t fingerprint[CLIENT_AUTH_FINGERPRINT_BYTES],
                             const uint8_t *message, size_t message_len,
                             const uint8_t *signature_der, size_t signature_len);
esp_err_t client_auth_revoke(const uint8_t fingerprint[CLIENT_AUTH_FINGERPRINT_BYTES]);
esp_err_t client_auth_revoke_all(void);
