#include "client_auth.h"

#include "mbedtls/platform_util.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "psa/crypto.h"
#include <stdbool.h>
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "transport.h"
#include "signaling.h"

#define AUTH_NAMESPACE "kvmctl_auth"
#define AUTH_KEY "clients"
#define AUTH_MAGIC 0x4b564d43u
#define AUTH_VERSION 1

typedef struct {
    uint32_t magic;
    uint16_t version;
    uint16_t count;
    uint8_t spki[CLIENT_AUTH_MAX_CLIENTS][CLIENT_AUTH_SPKI_BYTES];
} client_store_t;

static const uint8_t s_spki_prefix[] = {
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
    0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
};

static client_store_t s_store;
static bool s_revoke_persistence_pending;
static bool s_ready;
static SemaphoreHandle_t s_lock;

static psa_status_t import_public_key(const uint8_t *spki, psa_key_id_t *key)
{
    psa_key_attributes_t attributes = PSA_KEY_ATTRIBUTES_INIT;
    psa_set_key_type(&attributes, PSA_KEY_TYPE_ECC_PUBLIC_KEY(PSA_ECC_FAMILY_SECP_R1));
    psa_set_key_bits(&attributes, 256);
    psa_set_key_usage_flags(&attributes, PSA_KEY_USAGE_VERIFY_HASH);
    psa_set_key_algorithm(&attributes, PSA_ALG_ECDSA(PSA_ALG_SHA_256));
    psa_status_t result = psa_import_key(&attributes, spki + sizeof(s_spki_prefix), 65, key);
    psa_reset_key_attributes(&attributes);
    return result;
}

static esp_err_t fingerprint_spki(const uint8_t *spki, uint8_t fingerprint[32])
{
    if (!spki || !fingerprint || memcmp(spki, s_spki_prefix, sizeof(s_spki_prefix)) != 0 ||
        spki[sizeof(s_spki_prefix)] != 0x04) return ESP_ERR_INVALID_ARG;

    psa_key_id_t key = 0;
    psa_status_t imported = import_public_key(spki, &key);
    if (imported != PSA_SUCCESS) return ESP_ERR_INVALID_ARG;
    psa_destroy_key(key);

    size_t hash_len = 0;
    return psa_hash_compute(PSA_ALG_SHA_256, spki, CLIENT_AUTH_SPKI_BYTES,
                            fingerprint, 32, &hash_len) == PSA_SUCCESS && hash_len == 32
        ? ESP_OK : ESP_FAIL;
}

static esp_err_t persist(void)
{
    nvs_handle_t nvs;
    esp_err_t err = nvs_open(AUTH_NAMESPACE, NVS_READWRITE, &nvs);
    if (err != ESP_OK) return err;
    err = nvs_set_blob(nvs, AUTH_KEY, &s_store, sizeof(s_store));
    if (err == ESP_OK) err = nvs_commit(nvs);
    if (err == ESP_OK) {
        client_store_t check;
        size_t len = sizeof(check);
        err = nvs_get_blob(nvs, AUTH_KEY, &check, &len);
        if (err == ESP_OK && (len != sizeof(check) || memcmp(&check, &s_store, sizeof(check)) != 0)) {
            err = ESP_ERR_INVALID_STATE;
        }
        mbedtls_platform_zeroize(&check, sizeof(check));
    }
    nvs_close(nvs);
    return err;
}

esp_err_t client_auth_init(void)
{
    if (s_ready) return ESP_OK;
    s_lock = xSemaphoreCreateMutex();
    if (!s_lock) return ESP_ERR_NO_MEM;
    nvs_handle_t nvs;
    esp_err_t err = nvs_open(AUTH_NAMESPACE, NVS_READWRITE, &nvs);
    if (err != ESP_OK) return err;
    size_t len = sizeof(s_store);
    err = nvs_get_blob(nvs, AUTH_KEY, &s_store, &len);
    if (err == ESP_ERR_NVS_NOT_FOUND) {
        memset(&s_store, 0, sizeof(s_store));
        s_store.magic = AUTH_MAGIC;
        s_store.version = AUTH_VERSION;
        err = nvs_set_blob(nvs, AUTH_KEY, &s_store, sizeof(s_store));
        if (err == ESP_OK) err = nvs_commit(nvs);
    } else if (err == ESP_OK && (len != sizeof(s_store) || s_store.magic != AUTH_MAGIC ||
                                 s_store.version != AUTH_VERSION ||
                                 s_store.count > CLIENT_AUTH_MAX_CLIENTS)) {
        err = ESP_ERR_INVALID_STATE;
    }
    nvs_close(nvs);
    if (err == ESP_OK) {
        for (size_t i = 0; i < s_store.count; ++i) {
            uint8_t fingerprint[32];
            err = fingerprint_spki(s_store.spki[i], fingerprint);
            if (err != ESP_OK) break;
            for (size_t j = 0; j < i; ++j) {
                uint8_t prior[32];
                err = fingerprint_spki(s_store.spki[j], prior);
                if (err != ESP_OK || memcmp(prior, fingerprint, sizeof(prior)) == 0) {
                    err = ESP_ERR_INVALID_STATE;
                    break;
                }
            }
            if (err != ESP_OK) break;
        }
    }
    s_ready = err == ESP_OK;
    return err;
}

esp_err_t client_auth_enroll(const uint8_t *spki, size_t spki_len, uint8_t fingerprint[32])
{
    if (!s_ready || !fingerprint || spki_len != CLIENT_AUTH_SPKI_BYTES) return ESP_ERR_INVALID_ARG;
    xSemaphoreTake(s_lock, portMAX_DELAY);
    esp_err_t err = fingerprint_spki(spki, fingerprint);
    if (err != ESP_OK) goto done;
    size_t insert = s_store.count;
    for (size_t i = 0; i < s_store.count; ++i) {
        uint8_t current[32];
        err = fingerprint_spki(s_store.spki[i], current);
        if (err != ESP_OK) goto done;
        if (memcmp(current, fingerprint, sizeof(current)) == 0) {
            err = ESP_ERR_INVALID_STATE;
            goto done;
        }
    }
    if (insert == CLIENT_AUTH_MAX_CLIENTS) {
        err = ESP_ERR_NO_MEM;
        goto done;
    }
    memcpy(s_store.spki[insert], spki, CLIENT_AUTH_SPKI_BYTES);
    ++s_store.count;
    err = persist();
    if (err != ESP_OK) {
        --s_store.count;
        memset(s_store.spki[s_store.count], 0, CLIENT_AUTH_SPKI_BYTES);
    } else {
        s_revoke_persistence_pending = false;
    }
done:
    xSemaphoreGive(s_lock);
    return err;
}

esp_err_t client_auth_list(uint8_t fingerprints[CLIENT_AUTH_MAX_CLIENTS][32], size_t *count)
{
    if (!s_ready || !fingerprints || !count) return ESP_ERR_INVALID_STATE;
    xSemaphoreTake(s_lock, portMAX_DELAY);
    esp_err_t result = ESP_OK;
    for (size_t i = 0; i < s_store.count; ++i) {
        result = fingerprint_spki(s_store.spki[i], fingerprints[i]);
        if (result != ESP_OK) break;
    }
    if (result == ESP_OK) *count = s_store.count;
    xSemaphoreGive(s_lock);
    return result;
}

bool client_auth_is_enrolled(const uint8_t fingerprint[32])
{
    if (!s_ready || !fingerprint) return false;
    bool found = false;
    xSemaphoreTake(s_lock, portMAX_DELAY);
    for (size_t i = 0; i < s_store.count; ++i) {
        uint8_t current[32];
        if (fingerprint_spki(s_store.spki[i], current) == ESP_OK &&
            memcmp(current, fingerprint, sizeof(current)) == 0) {
            found = true;
            break;
        }
    }
    xSemaphoreGive(s_lock);
    return found;
}

static bool signature_der_to_raw(const uint8_t *der, size_t len, uint8_t raw[64])
{
    if (!der || len < 8 || len > 72 || der[0] != 0x30 || der[1] != len - 2) return false;
    size_t at = 2;
    for (size_t part = 0; part < 2; ++part) {
        if (at + 2 > len || der[at++] != 0x02) return false;
        size_t n = der[at++];
        if (!n || at + n > len || (der[at] & 0x80) ||
            (n > 1 && der[at] == 0 && !(der[at + 1] & 0x80))) return false;
        const uint8_t *value = der + at;
        if (n > 1 && value[0] == 0) { ++value; --n; }
        if (n > 32) return false;
        size_t offset = part * 32 + 32 - n;
        memset(raw + part * 32, 0, 32);
        memcpy(raw + offset, value, n);
        at += der[at - 1];
    }
    return at == len;
}

esp_err_t client_auth_verify(const uint8_t fingerprint[32], const uint8_t *message,
                             size_t message_len, const uint8_t *signature_der,
                             size_t signature_len)
{
    if (!s_ready || !fingerprint || !message || message_len == 0 || message_len > 16384 ||
        !signature_der || signature_len > 72) return ESP_ERR_INVALID_ARG;
    uint8_t spki[CLIENT_AUTH_SPKI_BYTES], raw_signature[64], hash[32];
    psa_key_id_t key = 0;
    size_t hash_len = 0;
    esp_err_t result = ESP_ERR_NOT_FOUND;
    if (!signature_der_to_raw(signature_der, signature_len, raw_signature)) return ESP_ERR_INVALID_ARG;
    xSemaphoreTake(s_lock, portMAX_DELAY);
    for (size_t i = 0; i < s_store.count; ++i) {
        uint8_t current[32];
        if (fingerprint_spki(s_store.spki[i], current) == ESP_OK &&
            memcmp(current, fingerprint, sizeof(current)) == 0) {
            memcpy(spki, s_store.spki[i], sizeof(spki));
            result = ESP_OK;
            break;
        }
    }
    xSemaphoreGive(s_lock);
    if (result == ESP_OK && psa_hash_compute(PSA_ALG_SHA_256, message, message_len,
                                              hash, sizeof(hash), &hash_len) == PSA_SUCCESS &&
        hash_len == sizeof(hash) && import_public_key(spki, &key) == PSA_SUCCESS) {
        psa_status_t verified = psa_verify_hash(key, PSA_ALG_ECDSA(PSA_ALG_SHA_256), hash,
                                                 sizeof(hash), raw_signature,
                                                 sizeof(raw_signature));
        result = verified == PSA_SUCCESS ? ESP_OK : ESP_ERR_INVALID_RESPONSE;
        psa_destroy_key(key);
    } else if (result == ESP_OK) {
        result = ESP_FAIL;
    }
    mbedtls_platform_zeroize(spki, sizeof(spki));
    mbedtls_platform_zeroize(raw_signature, sizeof(raw_signature));
    mbedtls_platform_zeroize(hash, sizeof(hash));
    return result;
}

esp_err_t client_auth_revoke(const uint8_t fingerprint[32])
{
    if (!s_ready || !fingerprint) return ESP_ERR_INVALID_STATE;
    xSemaphoreTake(s_lock, portMAX_DELAY);
    size_t index = s_store.count;
    for (size_t i = 0; i < s_store.count; ++i) {
        uint8_t current[32];
        esp_err_t err = fingerprint_spki(s_store.spki[i], current);
        if (err != ESP_OK) {
            xSemaphoreGive(s_lock);
            return err;
        }
        if (memcmp(current, fingerprint, sizeof(current)) == 0) {
            index = i;
            break;
        }
    }
    if (index == s_store.count) {
        esp_err_t result = ESP_ERR_NOT_FOUND;
        if (s_revoke_persistence_pending) {
            result = persist();
            if (result == ESP_OK) s_revoke_persistence_pending = false;
        }
        xSemaphoreGive(s_lock);
        return result;
    }
    if (index + 1 < s_store.count) {
        memmove(s_store.spki[index], s_store.spki[index + 1],
                (s_store.count - index - 1) * CLIENT_AUTH_SPKI_BYTES);
    }
    --s_store.count;
    memset(s_store.spki[s_store.count], 0, CLIENT_AUTH_SPKI_BYTES);
    transport_invalidate_client(fingerprint);
    signaling_invalidate_client(fingerprint);
    esp_err_t result;
#ifdef KVMCTL_TEST_REVOKE_PERSIST_FAIL_ONCE
    static bool test_revoke_persist_failure_injected;
    if (!test_revoke_persist_failure_injected) {
        test_revoke_persist_failure_injected = true;
        result = ESP_FAIL;
    } else
#endif
    {
        result = persist();
    }
    s_revoke_persistence_pending = result != ESP_OK;
    xSemaphoreGive(s_lock);
    return result;
}

esp_err_t client_auth_revoke_all(void)
{
    if (!s_ready) return ESP_ERR_INVALID_STATE;
    xSemaphoreTake(s_lock, portMAX_DELAY);
    memset(s_store.spki, 0, sizeof(s_store.spki));
    s_store.count = 0;
    transport_invalidate_all_clients();
    signaling_invalidate_all_clients();
    esp_err_t result = persist();
    s_revoke_persistence_pending = result != ESP_OK;
    xSemaphoreGive(s_lock);
    return result;
}
