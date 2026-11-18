#include "device_identity.h"

#include "mbedtls/platform_util.h"
#include "mbedtls/pk.h"
#include "mbedtls/x509_crt.h"
#include "esp_random.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "psa/crypto.h"
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

#define IDENTITY_NAMESPACE "kvmctl_id"
#define P256_PRIVATE_BYTES 32
#define P256_PUBLIC_BYTES 65
#define P256_SPKI_BYTES 91
#define TLS_CERT_PEM_BYTES 2048

static const uint8_t s_spki_prefix[] = {
    0x30, 0x59, 0x30, 0x13,
    0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
    0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
    0x03, 0x42, 0x00,
};

static psa_key_id_t s_identity_key;
static psa_key_id_t s_tls_key;
static bool s_ready;
static uint8_t s_tls_certificate[TLS_CERT_PEM_BYTES];

static esp_err_t load_or_create_key(nvs_handle_t nvs, const char *name,
                                    psa_algorithm_t signing_algorithm,
                                    psa_key_id_t *key_id)
{
    uint8_t private_key[P256_PRIVATE_BYTES];
    size_t len = sizeof(private_key);
    esp_err_t err = nvs_get_blob(nvs, name, private_key, &len);
    if (err == ESP_ERR_NVS_NOT_FOUND) {
        psa_key_attributes_t generated = PSA_KEY_ATTRIBUTES_INIT;
        psa_set_key_type(&generated, PSA_KEY_TYPE_ECC_KEY_PAIR(PSA_ECC_FAMILY_SECP_R1));
        psa_set_key_bits(&generated, 256);
        psa_set_key_usage_flags(&generated, PSA_KEY_USAGE_EXPORT | PSA_KEY_USAGE_SIGN_HASH);
        psa_set_key_algorithm(&generated, signing_algorithm);
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
    psa_set_key_algorithm(&imported, signing_algorithm);
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
    err = load_or_create_key(nvs, "endpoint", PSA_ALG_ECDSA(PSA_ALG_SHA_256),
                             &s_identity_key);
    if (err == ESP_OK) {
        /* Match Mbed TLS's configured ECDSA mode (deterministic when enabled). */
        err = load_or_create_key(nvs, "tls",
                                 MBEDTLS_PK_ALG_ECDSA(PSA_ALG_SHA_256),
                                 &s_tls_key);
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

esp_err_t device_identity_tls_material(const uint8_t **certificate_pem, size_t *certificate_len,
                                       uint32_t *private_key_id,
                                       char hostname[DEVICE_IDENTITY_TLS_NAME_MAX])
{
    if (!s_ready || !certificate_pem || !certificate_len || !private_key_id || !hostname) {
        return ESP_ERR_INVALID_STATE;
    }
    uint8_t endpoint_fingerprint[32], tls_hash[32], spki[DEVICE_IDENTITY_SPKI_MAX];
    size_t spki_len = 0;
    esp_err_t err = device_identity_status(endpoint_fingerprint, tls_hash, spki, &spki_len);
    if (err != ESP_OK) return err;
    int name_len = snprintf(hostname, DEVICE_IDENTITY_TLS_NAME_MAX, "kvmctl-%02x%02x%02x%02x%02x%02x%02x%02x.local",
                            endpoint_fingerprint[0], endpoint_fingerprint[1], endpoint_fingerprint[2],
                            endpoint_fingerprint[3], endpoint_fingerprint[4], endpoint_fingerprint[5],
                            endpoint_fingerprint[6], endpoint_fingerprint[7]);
    mbedtls_platform_zeroize(endpoint_fingerprint, sizeof(endpoint_fingerprint));
    mbedtls_platform_zeroize(tls_hash, sizeof(tls_hash));
    mbedtls_platform_zeroize(spki, sizeof(spki));
    if (name_len <= 0 || name_len >= DEVICE_IDENTITY_TLS_NAME_MAX) return ESP_ERR_INVALID_SIZE;

    mbedtls_pk_context key;
    mbedtls_pk_init(&key);
    if (mbedtls_pk_wrap_psa(&key, s_tls_key) != 0) return ESP_FAIL;

    mbedtls_x509write_cert crt;
    mbedtls_x509write_crt_init(&crt);
    mbedtls_x509write_crt_set_version(&crt, MBEDTLS_X509_CRT_VERSION_3);
    mbedtls_x509write_crt_set_md_alg(&crt, MBEDTLS_MD_SHA256);
    mbedtls_x509write_crt_set_subject_key(&crt, &key);
    mbedtls_x509write_crt_set_issuer_key(&crt, &key);
    char subject[DEVICE_IDENTITY_TLS_NAME_MAX + 16];
    snprintf(subject, sizeof(subject), "O=KVMCTL,CN=%s", hostname);
    int written = mbedtls_x509write_crt_set_subject_name(&crt, subject);
    if (written == 0) written = mbedtls_x509write_crt_set_issuer_name(&crt, subject);
    uint8_t serial[16];
    esp_fill_random(serial, sizeof(serial));
    serial[0] &= 0x7f;
    if (memcmp(serial, (uint8_t[16]){0}, sizeof(serial)) == 0) serial[sizeof(serial) - 1] = 1;
    if (written == 0) written = mbedtls_x509write_crt_set_serial_raw(&crt, serial, sizeof(serial));
    if (written == 0) written = mbedtls_x509write_crt_set_validity(&crt, "20260101000000", "20361231235959");
    if (written == 0) written = mbedtls_x509write_crt_set_basic_constraints(&crt, 0, -1);
    if (written == 0) written = mbedtls_x509write_crt_set_key_usage(&crt, MBEDTLS_X509_KU_DIGITAL_SIGNATURE);
    mbedtls_x509_san_list san = {0};
    san.node.type = MBEDTLS_X509_SAN_DNS_NAME;
    san.node.san.unstructured_name.tag = MBEDTLS_ASN1_IA5_STRING;
    san.node.san.unstructured_name.p = (unsigned char *)hostname;
    san.node.san.unstructured_name.len = strlen(hostname);
    if (written == 0) written = mbedtls_x509write_crt_set_subject_alternative_name(&crt, &san);
    if (written == 0) written = mbedtls_x509write_crt_set_subject_key_identifier(&crt);
    if (written == 0) written = mbedtls_x509write_crt_pem(&crt, s_tls_certificate,
                                                            sizeof(s_tls_certificate));
    mbedtls_x509write_crt_free(&crt);
    mbedtls_pk_free(&key);
    mbedtls_platform_zeroize(serial, sizeof(serial));
    if (written != 0) {
        mbedtls_platform_zeroize(s_tls_certificate, sizeof(s_tls_certificate));
        return ESP_FAIL;
    }
    *certificate_pem = s_tls_certificate;
    *certificate_len = strlen((const char *)s_tls_certificate) + 1;
    *private_key_id = (uint32_t)s_tls_key;
    return ESP_OK;
}

static size_t write_der_integer(uint8_t *output, const uint8_t value[32])
{
    size_t start = 0;
    while (start < 31 && value[start] == 0) ++start;
    size_t len = 32 - start;
    bool pad = (value[start] & 0x80) != 0;
    output[0] = 0x02;
    output[1] = (uint8_t)(len + (pad ? 1 : 0));
    if (pad) output[2] = 0;
    memcpy(output + 2 + (pad ? 1 : 0), value + start, len);
    return 2 + len + (pad ? 1 : 0);
}

esp_err_t device_identity_sign(const uint8_t *message, size_t message_len,
                               uint8_t *signature_der, size_t signature_capacity,
                               size_t *signature_len)
{
    if (!s_ready || !message || message_len == 0 || message_len > 16384 || !signature_der ||
        !signature_len || signature_capacity < 72) return ESP_ERR_INVALID_ARG;
    uint8_t hash[32], raw[64];
    size_t hash_len = 0, raw_len = 0;
    psa_status_t status = psa_hash_compute(PSA_ALG_SHA_256, message, message_len,
                                           hash, sizeof(hash), &hash_len);
    if (status == PSA_SUCCESS && hash_len == sizeof(hash)) {
        status = psa_sign_hash(s_identity_key, PSA_ALG_ECDSA(PSA_ALG_SHA_256),
                               hash, sizeof(hash), raw, sizeof(raw), &raw_len);
    }
    esp_err_t result = ESP_FAIL;
    if (status == PSA_SUCCESS && raw_len == sizeof(raw)) {
        uint8_t body[70];
        size_t body_len = write_der_integer(body, raw);
        body_len += write_der_integer(body + body_len, raw + 32);
        if (signature_capacity >= body_len + 2) {
            signature_der[0] = 0x30;
            signature_der[1] = (uint8_t)body_len;
            memcpy(signature_der + 2, body, body_len);
            *signature_len = body_len + 2;
            result = ESP_OK;
        } else {
            result = ESP_ERR_INVALID_SIZE;
        }
        mbedtls_platform_zeroize(body, sizeof(body));
    }
    mbedtls_platform_zeroize(hash, sizeof(hash));
    mbedtls_platform_zeroize(raw, sizeof(raw));
    return result;
}
