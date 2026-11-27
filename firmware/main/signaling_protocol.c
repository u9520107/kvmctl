#include "signaling_protocol.h"

#include "client_auth.h"
#include "device_identity.h"
#include "mbedtls/platform_util.h"
#include "protocol_vectors.h"
#include "psa/crypto.h"
#include <stdbool.h>
#include <string.h>

#define SIGNAL_CBOR_MAX 1024

static bool put_head(uint8_t *out, size_t capacity, size_t *pos, uint8_t major, uint64_t value)
{
    if (value <= 23) {
        if (*pos >= capacity) return false;
        out[(*pos)++] = (uint8_t)((major << 5) | value);
    } else if (value <= 0xff) {
        if (capacity - *pos < 2) return false;
        out[(*pos)++] = (uint8_t)((major << 5) | 24);
        out[(*pos)++] = (uint8_t)value;
    } else if (value <= 0xffff) {
        if (capacity - *pos < 3) return false;
        out[(*pos)++] = (uint8_t)((major << 5) | 25);
        out[(*pos)++] = (uint8_t)(value >> 8);
        out[(*pos)++] = (uint8_t)value;
    } else if (value <= 0xffffffff) {
        if (capacity - *pos < 5) return false;
        out[(*pos)++] = (uint8_t)((major << 5) | 26);
        out[(*pos)++] = (uint8_t)(value >> 24);
        out[(*pos)++] = (uint8_t)(value >> 16);
        out[(*pos)++] = (uint8_t)(value >> 8);
        out[(*pos)++] = (uint8_t)value;
    } else {
        if (capacity - *pos < 9) return false;
        out[(*pos)++] = (uint8_t)((major << 5) | 27);
        for (int shift = 56; shift >= 0; shift -= 8) out[(*pos)++] = (uint8_t)(value >> shift);
    }
    return true;
}

static bool put_uint(uint8_t *out, size_t capacity, size_t *pos, uint64_t value)
{
    return put_head(out, capacity, pos, 0, value);
}

static bool put_bytes(uint8_t *out, size_t capacity, size_t *pos, const uint8_t *value, size_t len)
{
    if (!value || len > capacity - *pos || !put_head(out, capacity, pos, 2, len) || len > capacity - *pos) {
        return false;
    }
    memcpy(out + *pos, value, len);
    *pos += len;
    return true;
}

static bool put_text(uint8_t *out, size_t capacity, size_t *pos, const char *value)
{
    size_t len = strlen(value);
    if (len == 0 || len > 32 || !put_head(out, capacity, pos, 3, len) || len > capacity - *pos) return false;
    memcpy(out + *pos, value, len);
    *pos += len;
    return true;
}

static bool put_capabilities(uint8_t *out, size_t capacity, size_t *pos)
{
    return put_head(out, capacity, pos, 4, 3) && put_text(out, capacity, pos, "control") &&
           put_text(out, capacity, pos, "input") && put_text(out, capacity, pos, "motion");
}

esp_err_t signaling_encode_challenge(const signaling_challenge_t *c,
                                     uint8_t *out, size_t capacity, size_t *out_len)
{
    if (!c || !out || !out_len) return ESP_ERR_INVALID_ARG;
    size_t pos = 0;
    bool ok = put_head(out, capacity, &pos, 5, 9) &&
        put_uint(out, capacity, &pos, 0) && put_uint(out, capacity, &pos, 1) &&
        put_uint(out, capacity, &pos, 1) && put_uint(out, capacity, &pos, 0) &&
        put_uint(out, capacity, &pos, 2) && put_bytes(out, capacity, &pos, c->session_id, 16) &&
        put_uint(out, capacity, &pos, 3) && put_bytes(out, capacity, &pos, c->client_fingerprint, 32) &&
        put_uint(out, capacity, &pos, 4) && put_bytes(out, capacity, &pos, c->endpoint_fingerprint, 32) &&
        put_uint(out, capacity, &pos, 5) && put_bytes(out, capacity, &pos, c->client_nonce, 32) &&
        put_uint(out, capacity, &pos, 6) && put_bytes(out, capacity, &pos, c->device_nonce, 32) &&
        put_uint(out, capacity, &pos, 7) && put_capabilities(out, capacity, &pos) &&
        put_uint(out, capacity, &pos, 8) && put_bytes(out, capacity, &pos, c->tls_spki_hash, 32);
    if (!ok) return ESP_ERR_INVALID_SIZE;
    *out_len = pos;
    return ESP_OK;
}

esp_err_t signaling_encode_transcript(const signaling_transcript_t *t,
                                      uint8_t *out, size_t capacity, size_t *out_len)
{
    if (!t || !out || !out_len) return ESP_ERR_INVALID_ARG;
    size_t pos = 0;
    const signaling_challenge_t *c = &t->challenge;
    bool ok = put_head(out, capacity, &pos, 5, 13) &&
        put_uint(out, capacity, &pos, 0) && put_uint(out, capacity, &pos, 1) &&
        put_uint(out, capacity, &pos, 1) && put_uint(out, capacity, &pos, 0) &&
        put_uint(out, capacity, &pos, 2) && put_bytes(out, capacity, &pos, c->session_id, 16) &&
        put_uint(out, capacity, &pos, 3) && put_bytes(out, capacity, &pos, c->client_fingerprint, 32) &&
        put_uint(out, capacity, &pos, 4) && put_bytes(out, capacity, &pos, c->endpoint_fingerprint, 32) &&
        put_uint(out, capacity, &pos, 5) && put_bytes(out, capacity, &pos, c->client_nonce, 32) &&
        put_uint(out, capacity, &pos, 6) && put_bytes(out, capacity, &pos, c->device_nonce, 32) &&
        put_uint(out, capacity, &pos, 7) && put_capabilities(out, capacity, &pos) &&
        put_uint(out, capacity, &pos, 8) && put_bytes(out, capacity, &pos, c->tls_spki_hash, 32) &&
        put_uint(out, capacity, &pos, 9) && put_bytes(out, capacity, &pos, t->offer_hash, 32) &&
        put_uint(out, capacity, &pos, 10) && put_bytes(out, capacity, &pos, t->answer_hash, 32) &&
        put_uint(out, capacity, &pos, 11) && put_bytes(out, capacity, &pos, t->client_dtls_fingerprint, 32) &&
        put_uint(out, capacity, &pos, 12) && put_bytes(out, capacity, &pos, t->endpoint_dtls_fingerprint, 32);
    if (!ok) return ESP_ERR_INVALID_SIZE;
    *out_len = pos;
    return ESP_OK;
}

static esp_err_t domain_message(const char *domain, const uint8_t *encoded, size_t encoded_len,
                               uint8_t message[SIGNAL_CBOR_MAX], size_t *message_len)
{
    size_t domain_len = strlen(domain);
    if (!encoded || !message_len || domain_len + encoded_len > SIGNAL_CBOR_MAX) return ESP_ERR_INVALID_SIZE;
    memcpy(message, domain, domain_len);
    memcpy(message + domain_len, encoded, encoded_len);
    *message_len = domain_len + encoded_len;
    return ESP_OK;
}

static esp_err_t sign_encoded(const char *domain, const uint8_t *encoded, size_t encoded_len,
                              uint8_t *signature, size_t capacity, size_t *signature_len)
{
    uint8_t message[SIGNAL_CBOR_MAX];
    size_t len = 0;
    esp_err_t err = domain_message(domain, encoded, encoded_len, message, &len);
    if (err == ESP_OK) err = device_identity_sign(message, len, signature, capacity, signature_len);
    mbedtls_platform_zeroize(message, sizeof(message));
    return err;
}

esp_err_t signaling_sign_challenge(const signaling_challenge_t *challenge,
                                   uint8_t *signature_der, size_t capacity, size_t *signature_len)
{
    uint8_t encoded[SIGNAL_CBOR_MAX];
    size_t len = 0;
    esp_err_t err = signaling_encode_challenge(challenge, encoded, sizeof(encoded), &len);
    if (err == ESP_OK) err = sign_encoded(KVMCTL_M13_CHALLENGE_DOMAIN, encoded, len,
                                         signature_der, capacity, signature_len);
    mbedtls_platform_zeroize(encoded, sizeof(encoded));
    return err;
}

esp_err_t signaling_verify_client_challenge(const signaling_challenge_t *challenge,
                                            const uint8_t *signature_der, size_t signature_len)
{
    uint8_t encoded[SIGNAL_CBOR_MAX], message[SIGNAL_CBOR_MAX];
    size_t len = 0, message_len = 0;
    esp_err_t err = signaling_encode_challenge(challenge, encoded, sizeof(encoded), &len);
    if (err == ESP_OK) err = domain_message(KVMCTL_M13_CLIENT_PROOF_DOMAIN, encoded, len, message, &message_len);
    if (err == ESP_OK) err = client_auth_verify(challenge->client_fingerprint, message, message_len,
                                                signature_der, signature_len);
    mbedtls_platform_zeroize(encoded, sizeof(encoded));
    mbedtls_platform_zeroize(message, sizeof(message));
    return err;
}

esp_err_t signaling_sign_transcript(const signaling_transcript_t *transcript,
                                    uint8_t *signature_der, size_t capacity, size_t *signature_len)
{
    uint8_t encoded[SIGNAL_CBOR_MAX];
    size_t len = 0;
    esp_err_t err = signaling_encode_transcript(transcript, encoded, sizeof(encoded), &len);
    if (err == ESP_OK) err = sign_encoded("kvmctl/transcript/v1", encoded, len,
                                         signature_der, capacity, signature_len);
    mbedtls_platform_zeroize(encoded, sizeof(encoded));
    return err;
}

esp_err_t signaling_verify_client_transcript(const signaling_transcript_t *transcript,
                                             const uint8_t *signature_der, size_t signature_len)
{
    uint8_t encoded[SIGNAL_CBOR_MAX], message[SIGNAL_CBOR_MAX];
    size_t len = 0, message_len = 0;
    esp_err_t err = signaling_encode_transcript(transcript, encoded, sizeof(encoded), &len);
    if (err == ESP_OK) err = domain_message("kvmctl/transcript/v1", encoded, len, message, &message_len);
    if (err == ESP_OK) err = client_auth_verify(transcript->challenge.client_fingerprint, message,
                                                message_len, signature_der, signature_len);
    mbedtls_platform_zeroize(encoded, sizeof(encoded));
    mbedtls_platform_zeroize(message, sizeof(message));
    return err;
}

static int hex_value(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    return -1;
}

static bool matches_hex(const uint8_t *data, size_t len, const char *expected)
{
    if (strlen(expected) != len * 2) return false;
    for (size_t i = 0; i < len; ++i) {
        int high = hex_value(expected[i * 2]), low = hex_value(expected[i * 2 + 1]);
        if (high < 0 || low < 0 || data[i] != (uint8_t)((high << 4) | low)) return false;
    }
    return true;
}

esp_err_t signaling_check_shared_vectors(void)
{
    if (strcmp(KVMCTL_M13_CHALLENGE_DOMAIN, KVMCTL_M13_CLIENT_PROOF_DOMAIN) == 0) {
        return ESP_ERR_INVALID_STATE;
    }
    signaling_challenge_t challenge = {
        .session_id = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15},
    };
    memset(challenge.client_fingerprint, 0x11, sizeof(challenge.client_fingerprint));
    memset(challenge.endpoint_fingerprint, 0x22, sizeof(challenge.endpoint_fingerprint));
    memset(challenge.client_nonce, 0x33, sizeof(challenge.client_nonce));
    memset(challenge.device_nonce, 0x44, sizeof(challenge.device_nonce));
    memset(challenge.tls_spki_hash, 0x55, sizeof(challenge.tls_spki_hash));
    uint8_t encoded[SIGNAL_CBOR_MAX];
    size_t len = 0;
    esp_err_t err = signaling_encode_challenge(&challenge, encoded, sizeof(encoded), &len);
    if (err != ESP_OK || !matches_hex(encoded, len, KVMCTL_M13_CHALLENGE_HEX)) return ESP_ERR_INVALID_STATE;
    signaling_transcript_t transcript = {.challenge = challenge};
    size_t hash_len = 0;
    if (psa_hash_compute(PSA_ALG_SHA_256, (const uint8_t *)"offer", 5,
                         transcript.offer_hash, sizeof(transcript.offer_hash), &hash_len) != PSA_SUCCESS ||
        hash_len != sizeof(transcript.offer_hash) ||
        psa_hash_compute(PSA_ALG_SHA_256, (const uint8_t *)"answer", 6,
                         transcript.answer_hash, sizeof(transcript.answer_hash), &hash_len) != PSA_SUCCESS ||
        hash_len != sizeof(transcript.answer_hash)) return ESP_FAIL;
    err = signaling_encode_transcript(&transcript, encoded, sizeof(encoded), &len);
    bool transcript_matches = err == ESP_OK && matches_hex(encoded, len, KVMCTL_M13_TRANSCRIPT_HEX);
    mbedtls_platform_zeroize(encoded, sizeof(encoded));
    return transcript_matches ? ESP_OK : ESP_ERR_INVALID_STATE;
}
