#pragma once

#include "esp_err.h"
#include <stddef.h>
#include <stdint.h>

typedef struct {
    uint8_t session_id[16];
    uint8_t client_fingerprint[32];
    uint8_t endpoint_fingerprint[32];
    uint8_t client_nonce[32];
    uint8_t device_nonce[32];
    uint8_t tls_spki_hash[32];
} signaling_challenge_t;

typedef struct {
    signaling_challenge_t challenge;
    uint8_t offer_hash[32];
    uint8_t answer_hash[32];
    uint8_t client_dtls_fingerprint[32];
    uint8_t endpoint_dtls_fingerprint[32];
} signaling_transcript_t;

esp_err_t signaling_encode_challenge(const signaling_challenge_t *challenge,
                                     uint8_t *out, size_t capacity, size_t *out_len);
esp_err_t signaling_encode_transcript(const signaling_transcript_t *transcript,
                                      uint8_t *out, size_t capacity, size_t *out_len);
esp_err_t signaling_sign_challenge(const signaling_challenge_t *challenge,
                                   uint8_t *signature_der, size_t capacity, size_t *signature_len);
esp_err_t signaling_verify_client_challenge(const signaling_challenge_t *challenge,
                                            const uint8_t *signature_der, size_t signature_len);
esp_err_t signaling_sign_transcript(const signaling_transcript_t *transcript,
                                    uint8_t *signature_der, size_t capacity, size_t *signature_len);
esp_err_t signaling_verify_client_transcript(const signaling_transcript_t *transcript,
                                             const uint8_t *signature_der, size_t signature_len);
esp_err_t signaling_check_shared_vectors(void);
