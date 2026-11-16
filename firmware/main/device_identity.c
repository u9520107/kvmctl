#include "device_identity.h"

#include "mbedtls/platform_util.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "psa/crypto.h"
#include <stdbool.h>
#include <string.h>

#define IDENTITY_NAMESPACE "kvmctl_id"
#define P256_PRIVATE_BYTES 32
#define P256_PUBLIC_BYTES 65
#define P256_SPKI_BYTES 91

static const uint8_t s_spki_prefix[] = {
    0x30, 0x59, 0x30, 0x13,
    0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
    0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
    0x03, 0x42, 0x00,
};

static psa_key_id_t s_identity_key;
static psa_key_id_t s_tls_key;
static bool s_ready;

static esp_err_t load_or_create_key(nvs_handle_t nvs, const char *name, psa_key_id_t *key_id)
{
    uint8_t private_key[P256_PRIVATE_BYTES];
    size_t len = sizeof(private_key);
    esp_err_t err = nvs_get_blob(nvs, name, private_key, &len);
    if (err == ESP_ERR_NVS_NOT_FOUND) {
        psa_key_attributes_t generated = PSA_KEY_ATTRIBUTES_INIT;
        psa_set_key_type(&generated, PSA_KEY_TYPE_ECC_KEY_PAIR(PSA_ECC_FAMILY_SECP_R1));
        psa_set_key_bits(&generated, 256);
        psa_set_key_usage_flags(&generated, PSA_KEY_USAGE_EXPORT | PSA_KEY_USAGE_SIGN_HASH);
        psa_set_key_algorithm(&generated, PSA_ALG_ECDSA(PSA_ALG_SHA_256));
        psa_status_t status = psa_generate_key(&generated, key_id);
        psa_reset_key_attributes(&generated);
        if (status != PSA_SUCCESS) {
            return ESP_FAIL;
        }
        status = psa_export_key(*key_id, private_key, sizeof(private_key), &len);
        if (status != PSA_SUCCESS || len != sizeof(private_key)) {
            psa_destroy_key(*key_id);
            mbedtls_platform_zeroize(private_key, sizeof(private_key));
            return ESP_FAIL;
        }
        err = nvs_set_blob(nvs, name, private_key, len);
        psa_destroy_key(*key_id);
        if (err != ESP_OK) {
            mbedtls_platform_zeroize(private_key, sizeof(private_key));
            return err;
        }
    }
    if (err != ESP_OK || len != sizeof(private_key)) {
        mbedtls_platform_zeroize(private_key, sizeof(private_key));
        return err == ESP_OK ? ESP_ERR_INVALID_SIZE : err;
    }

    psa_key_attributes_t imported = PSA_KEY_ATTRIBUTES_INIT;
    psa_set_key_type(&imported, PSA_KEY_TYPE_ECC_KEY_PAIR(PSA_ECC_FAMILY_SECP_R1));
    psa_set_key_bits(&imported, 256);
    psa_set_key_usage_flags(&imported, PSA_KEY_USAGE_SIGN_HASH);
    psa_set_key_algorithm(&imported, PSA_ALG_ECDSA(PSA_ALG_SHA_256));
    psa_status_t status = psa_import_key(&imported, private_key, len, key_id);
    psa_reset_key_attributes(&imported);
    mbedtls_platform_zeroize(private_key, sizeof(private_key));
    return status == PSA_SUCCESS ? ESP_OK : ESP_FAIL;
}

esp_err_t device_identity_init(void)
{
    if (s_ready) {
        return ESP_OK;
    }
    if (psa_crypto_init() != PSA_SUCCESS) {
        return ESP_FAIL;
    }
    esp_err_t err = nvs_flash_init();
    if (err != ESP_OK) {
        return err;
    }
    nvs_handle_t nvs;
    err = nvs_open(IDENTITY_NAMESPACE, NVS_READWRITE, &nvs);
    if (err != ESP_OK) {
        return err;
    }
    err = load_or_create_key(nvs, "endpoint", &s_identity_key);
    if (err == ESP_OK) {
        err = load_or_create_key(nvs, "tls", &s_tls_key);
    }
    if (err == ESP_OK) {
        err = nvs_commit(nvs);
    }
    nvs_close(nvs);
    if (err != ESP_OK) {
        if (s_identity_key != 0) psa_destroy_key(s_identity_key);
        if (s_tls_key != 0) psa_destroy_key(s_tls_key);
        s_identity_key = 0;
        s_tls_key = 0;
        return err;
    }
    s_ready = true;
    return ESP_OK;
}

static esp_err_t export_spki_and_hash(psa_key_id_t key_id, uint8_t *spki, uint8_t hash[32])
{
    size_t public_len = 0;
    if (psa_export_public_key(key_id, spki + sizeof(s_spki_prefix), P256_PUBLIC_BYTES, &public_len) != PSA_SUCCESS ||
        public_len != P256_PUBLIC_BYTES || spki[sizeof(s_spki_prefix)] != 0x04) {
        return ESP_FAIL;
    }
    memcpy(spki, s_spki_prefix, sizeof(s_spki_prefix));
    size_t hash_len = 0;
    if (psa_hash_compute(PSA_ALG_SHA_256, spki, P256_SPKI_BYTES, hash, 32, &hash_len) != PSA_SUCCESS || hash_len != 32) {
        return ESP_FAIL;
    }
    return ESP_OK;
}

esp_err_t device_identity_status(uint8_t fingerprint[32], uint8_t tls_spki_hash[32],
                                 uint8_t spki_der[DEVICE_IDENTITY_SPKI_MAX], size_t *spki_len)
{
    if (!s_ready || fingerprint == NULL || tls_spki_hash == NULL || spki_der == NULL || spki_len == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    if (DEVICE_IDENTITY_SPKI_MAX < P256_SPKI_BYTES) {
        return ESP_ERR_INVALID_SIZE;
    }
    esp_err_t err = export_spki_and_hash(s_identity_key, spki_der, fingerprint);
    if (err == ESP_OK) {
        uint8_t tls_spki[P256_SPKI_BYTES];
        err = export_spki_and_hash(s_tls_key, tls_spki, tls_spki_hash);
        mbedtls_platform_zeroize(tls_spki, sizeof(tls_spki));
    }
    if (err == ESP_OK) {
        *spki_len = P256_SPKI_BYTES;
    }
    return err;
}
