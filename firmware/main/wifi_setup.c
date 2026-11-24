#include "wifi_setup.h"

#include "esp_event.h"
#include "esp_netif.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "mbedtls/platform_util.h"
#include "nvs.h"
#include "recovery.h"
#include <stdbool.h>
#include <string.h>

#define WIFI_NAMESPACE "kvmctl_wifi"

static uint8_t s_state;
static bool s_initialized;
static bool s_started;
static esp_timer_handle_t s_reconnect_timer;
static uint8_t s_retry_step;

static void schedule_reconnect(void);

static void reconnect_timer_callback(void *arg)
{
    (void)arg;
    esp_err_t err = esp_wifi_connect();
    if (err == ESP_OK) {
        s_state = 1;
    } else {
        s_state = 3;
        schedule_reconnect();
    }
}

static void schedule_reconnect(void)
{
    if (s_reconnect_timer == NULL) {
        s_state = 3;
        return;
    }
    if (esp_timer_is_active(s_reconnect_timer)) {
        s_state = 1;
        return;
    }

    static const uint64_t delays_us[] = {
        1000000, 2000000, 4000000, 8000000, 16000000, 30000000,
    };
    size_t index = s_retry_step;
    if (index >= sizeof(delays_us) / sizeof(delays_us[0]) - 1) {
        index = sizeof(delays_us) / sizeof(delays_us[0]) - 1;
    } else {
        ++s_retry_step;
    }
    s_state = esp_timer_start_once(s_reconnect_timer, delays_us[index]) == ESP_OK ? 1 : 3;
}

static void reset_reconnect(void)
{
    s_retry_step = 0;
    if (s_reconnect_timer != NULL && esp_timer_is_active(s_reconnect_timer)) {
        esp_timer_stop(s_reconnect_timer);
    }
}

static bool valid_passphrase(const uint8_t *passphrase, size_t len)
{
    if (len >= 8 && len <= 63) {
        for (size_t i = 0; i < len; ++i) {
            if (passphrase[i] < 0x20 || passphrase[i] > 0x7e) return false;
        }
        return true;
    }
    if (len != 64) return false;
    for (size_t i = 0; i < len; ++i) {
        const uint8_t c = passphrase[i];
        if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'))) return false;
    }
    return true;
}

static bool valid_utf8(const uint8_t *data, size_t len)
{
    size_t i = 0;
    while (i < len) {
        uint8_t c = data[i++];
        if (c <= 0x7f) continue;
        uint32_t codepoint;
        size_t extra;
        if (c >= 0xc2 && c <= 0xdf) { codepoint = c & 0x1f; extra = 1; }
        else if (c >= 0xe0 && c <= 0xef) { codepoint = c & 0x0f; extra = 2; }
        else if (c >= 0xf0 && c <= 0xf4) { codepoint = c & 0x07; extra = 3; }
        else return false;
        if (extra > len - i) return false;
        for (size_t j = 0; j < extra; ++j) {
            uint8_t next = data[i++];
            if ((next & 0xc0) != 0x80) return false;
            codepoint = (codepoint << 6) | (next & 0x3f);
        }
        if ((extra == 1 && codepoint < 0x80) || (extra == 2 && codepoint < 0x800) ||
            (extra == 3 && codepoint < 0x10000) || codepoint > 0x10ffff ||
            (codepoint >= 0xd800 && codepoint <= 0xdfff)) return false;
    }
    return true;
}

static void wifi_event_handler(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    (void)arg;
    (void)data;
    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_START) {
        s_state = 1;
        if (esp_wifi_connect() != ESP_OK) {
            s_state = 3;
            schedule_reconnect();
        }
    } else if (base == WIFI_EVENT && id == WIFI_EVENT_STA_CONNECTED) {
        s_state = 1;
        if (s_reconnect_timer != NULL && esp_timer_is_active(s_reconnect_timer)) {
            esp_timer_stop(s_reconnect_timer);
        }
    } else if (base == WIFI_EVENT && id == WIFI_EVENT_STA_DISCONNECTED) {
        s_state = 3;
        schedule_reconnect();
    } else if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        s_state = 2;
        reset_reconnect();
    }
}

static esp_err_t wifi_stack_init(void)
{
    if (s_initialized) return ESP_OK;
    esp_err_t err = esp_netif_init();
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) return err;
    err = esp_event_loop_create_default();
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) return err;
    if (esp_netif_create_default_wifi_sta() == NULL) return ESP_ERR_NO_MEM;

    wifi_init_config_t config = WIFI_INIT_CONFIG_DEFAULT();
    err = esp_wifi_init(&config);
    if (err != ESP_OK) return err;
    const esp_timer_create_args_t timer_args = {
        .callback = reconnect_timer_callback,
        .name = "wifi_reconnect",
    };
    err = esp_timer_create(&timer_args, &s_reconnect_timer);
    if (err != ESP_OK) return err;
    err = esp_wifi_set_storage(WIFI_STORAGE_RAM);
    if (err == ESP_OK) err = esp_wifi_set_mode(WIFI_MODE_STA);
    if (err == ESP_OK) err = esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, wifi_event_handler, NULL);
    if (err == ESP_OK) err = esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event_handler, NULL);
    if (err != ESP_OK) return err;
    s_initialized = true;
    return ESP_OK;
}

static esp_err_t apply_credentials(const uint8_t *ssid, size_t ssid_len,
                                   const uint8_t *passphrase, size_t passphrase_len)
{
    esp_err_t err = wifi_stack_init();
    if (err != ESP_OK) return err;
    wifi_config_t config = {0};
    memcpy(config.sta.ssid, ssid, ssid_len);
    memcpy(config.sta.password, passphrase, passphrase_len);
    config.sta.threshold.authmode = WIFI_AUTH_WPA2_PSK;
    if (s_started) {
        esp_wifi_disconnect();
    }
    err = esp_wifi_set_config(WIFI_IF_STA, &config);
    mbedtls_platform_zeroize(&config, sizeof(config));
    if (err != ESP_OK) return err;
    s_state = 1;
    if (!s_started) {
        err = esp_wifi_start();
        if (err == ESP_OK) s_started = true;
    } else {
        err = esp_wifi_connect();
    }
    if (err != ESP_OK) {
        s_state = 3;
        schedule_reconnect();
    }
    return err;
}

uint8_t wifi_setup_state(void)
{
    return s_state;
}

esp_err_t wifi_setup_init(void)
{
    /* The HTTPS server opens lwIP sockets even before Wi-Fi is provisioned. */
    esp_err_t err = wifi_stack_init();
    if (err != ESP_OK) {
        s_state = 3;
        return err;
    }

    nvs_handle_t nvs;
    err = nvs_open(WIFI_NAMESPACE, NVS_READONLY, &nvs);
    if (err == ESP_ERR_NVS_NOT_FOUND) return ESP_OK;
    if (err != ESP_OK) { s_state = 3; return err; }

    uint8_t ssid[32], passphrase[64];
    size_t ssid_len = sizeof(ssid), passphrase_len = sizeof(passphrase);
    err = nvs_get_blob(nvs, "ssid", ssid, &ssid_len);
    if (err == ESP_ERR_NVS_NOT_FOUND) {
        nvs_close(nvs);
        return ESP_OK;
    }
    if (err == ESP_OK) err = nvs_get_blob(nvs, "pass", passphrase, &passphrase_len);
    nvs_close(nvs);
    if (err != ESP_OK || ssid_len == 0 || ssid_len > sizeof(ssid) ||
        !valid_utf8(ssid, ssid_len) || !valid_passphrase(passphrase, passphrase_len)) {
        s_state = 3;
        mbedtls_platform_zeroize(passphrase, sizeof(passphrase));
        return ESP_OK;
    }
    err = apply_credentials(ssid, ssid_len, passphrase, passphrase_len);
    mbedtls_platform_zeroize(passphrase, sizeof(passphrase));
    if (err != ESP_OK) s_state = 3;
    return ESP_OK;
}

esp_err_t wifi_setup_provision(const uint8_t *ssid, size_t ssid_len,
                               const uint8_t *passphrase, size_t passphrase_len)
{
    if (ssid == NULL || passphrase == NULL || ssid_len == 0 || ssid_len > 32 ||
        !valid_utf8(ssid, ssid_len) || !valid_passphrase(passphrase, passphrase_len)) {
        return ESP_ERR_INVALID_ARG;
    }
    recovery_storage_write_lock();
    nvs_handle_t nvs;
    esp_err_t err = nvs_open(WIFI_NAMESPACE, NVS_READWRITE, &nvs);
    if (err != ESP_OK) {
        recovery_storage_write_unlock();
        return err;
    }
    err = nvs_set_blob(nvs, "ssid", ssid, ssid_len);
    if (err == ESP_OK) err = nvs_set_blob(nvs, "pass", passphrase, passphrase_len);
    if (err == ESP_OK) err = nvs_commit(nvs);
    nvs_close(nvs);
    if (err != ESP_OK) {
        recovery_storage_write_unlock();
        return err;
    }
    s_state = 1;
    err = apply_credentials(ssid, ssid_len, passphrase, passphrase_len);
    if (err != ESP_OK) s_state = 3;
    recovery_storage_write_unlock();
    return ESP_OK;
}
