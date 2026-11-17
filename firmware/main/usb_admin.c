#include "usb_admin.h"

#include "device_identity.h"
#include "driver/uart.h"
#include "esp_app_desc.h"
#include "esp_err.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "led_control.h"
#include "mbedtls/platform_util.h"
#include "wifi_setup.h"
#include <stdarg.h>
#include <stdbool.h>
#include <string.h>

#define ADMIN_UART UART_NUM_0
#define ADMIN_MAX_PAYLOAD 4096
#define ADMIN_VERSION_MAJOR 1
#define ADMIN_VERSION_MINOR 0
#define ADMIN_WIFI_SET 0x60
#define ADMIN_STATUS 0x61
#define ADMIN_LED_SET 0x66

typedef struct {
    const uint8_t *data;
    size_t len;
    size_t pos;
} cbor_reader_t;

typedef struct {
    uint64_t version_major;
    uint64_t version_minor;
    uint64_t command;
    uint8_t request_id[16];
    uint8_t ssid[32];
    size_t ssid_len;
    uint8_t passphrase[64];
    size_t passphrase_len;
    bool led_enabled;
    uint8_t led_brightness;
    uint8_t led_rgb[3];
} admin_request_t;

static uint8_t s_request[ADMIN_MAX_PAYLOAD];
static uint8_t s_response[ADMIN_MAX_PAYLOAD];
static uint8_t s_spki[DEVICE_IDENTITY_SPKI_MAX];

static int discard_log(const char *format, va_list args)
{
    (void)format;
    (void)args;
    return 0;
}

void usb_admin_silence_logs(void)
{
    esp_log_set_vprintf(discard_log);
}

static bool read_head(cbor_reader_t *r, uint8_t *major, uint64_t *value)
{
    if (r->pos >= r->len) return false;
    uint8_t initial = r->data[r->pos++];
    *major = initial >> 5;
    uint8_t ai = initial & 31;
    if (ai <= 23) { *value = ai; return true; }
    size_t n = ai == 24 ? 1 : ai == 25 ? 2 : ai == 26 ? 4 : ai == 27 ? 8 : 0;
    if (n == 0 || n > r->len - r->pos) return false;
    uint64_t v = 0;
    for (size_t i = 0; i < n; ++i) v = (v << 8) | r->data[r->pos++];
    if ((n == 1 && v < 24) || (n == 2 && v <= UINT8_MAX) ||
        (n == 4 && v <= UINT16_MAX) || (n == 8 && v <= UINT32_MAX)) return false;
    *value = v;
    return true;
}

static bool read_uint(cbor_reader_t *r, uint64_t *out)
{
    uint8_t major;
    return read_head(r, &major, out) && major == 0;
}

static bool read_expected_uint(cbor_reader_t *r, uint64_t expected)
{
    uint64_t value;
    return read_uint(r, &value) && value == expected;
}

static bool read_map(cbor_reader_t *r, uint64_t expected)
{
    uint8_t major;
    uint64_t len;
    return read_head(r, &major, &len) && major == 5 && len == expected;
}

static bool read_bytes(cbor_reader_t *r, uint8_t *out, size_t expected)
{
    uint8_t major;
    uint64_t len;
    if (!read_head(r, &major, &len) || major != 2 || len != expected || len > r->len - r->pos) return false;
    memcpy(out, r->data + r->pos, expected);
    r->pos += expected;
    return true;
}

static bool read_bounded_string(cbor_reader_t *r, uint8_t expected_major,
                               uint8_t *out, size_t capacity, size_t *out_len)
{
    uint8_t major;
    uint64_t len;
    if (!read_head(r, &major, &len) || major != expected_major || len == 0 ||
        len > capacity || len > r->len - r->pos) return false;
    memcpy(out, r->data + r->pos, (size_t)len);
    r->pos += (size_t)len;
    *out_len = (size_t)len;
    return true;
}

static bool read_bool(cbor_reader_t *r, bool *out)
{
    uint8_t major;
    uint64_t value;
    if (!read_head(r, &major, &value) || major != 7 || (value != 20 && value != 21)) return false;
    *out = value == 21;
    return true;
}

typedef enum {
    REQUEST_INVALID,
    REQUEST_UNSUPPORTED_VERSION,
    REQUEST_SUPPORTED,
} request_decode_result_t;

static request_decode_result_t decode_request(const uint8_t *data, size_t len,
                                              admin_request_t *request)
{
    cbor_reader_t r = { .data = data, .len = len };
    uint8_t major;
    uint64_t map_len;
    if (!read_head(&r, &major, &map_len) || major != 5 || map_len != 5 ||
        !read_expected_uint(&r, 0) || !read_uint(&r, &request->version_major) ||
        !read_expected_uint(&r, 1) || !read_uint(&r, &request->version_minor) ||
        !read_expected_uint(&r, 2) || !read_uint(&r, &request->command) ||
        !read_expected_uint(&r, 3) || !read_bytes(&r, request->request_id, sizeof(request->request_id)) ||
        !read_expected_uint(&r, 4)) return REQUEST_INVALID;

    if (request->version_major != ADMIN_VERSION_MAJOR ||
        request->version_minor != ADMIN_VERSION_MINOR) {
        return r.pos < r.len ? REQUEST_UNSUPPORTED_VERSION : REQUEST_INVALID;
    }

    if (request->command == ADMIN_STATUS) {
        return read_map(&r, 0) && r.pos == r.len ? REQUEST_SUPPORTED : REQUEST_INVALID;
    }
    if (request->command == ADMIN_WIFI_SET) {
        return read_map(&r, 2) && read_expected_uint(&r, 0) &&
               read_bounded_string(&r, 3, request->ssid, sizeof(request->ssid), &request->ssid_len) &&
               read_expected_uint(&r, 1) &&
               read_bounded_string(&r, 2, request->passphrase, sizeof(request->passphrase), &request->passphrase_len) &&
               r.pos == r.len ? REQUEST_SUPPORTED : REQUEST_INVALID;
    }
    if (request->command == ADMIN_LED_SET) {
        uint64_t brightness;
        return read_map(&r, 3) && read_expected_uint(&r, 0) && read_bool(&r, &request->led_enabled) &&
               read_expected_uint(&r, 1) && read_uint(&r, &brightness) && brightness <= 100 &&
               read_expected_uint(&r, 2) && read_bytes(&r, request->led_rgb, sizeof(request->led_rgb)) &&
               (request->led_brightness = (uint8_t)brightness, true) && r.pos == r.len
            ? REQUEST_SUPPORTED : REQUEST_INVALID;
    }
    return REQUEST_INVALID;
}

static bool put_head(uint8_t *out, size_t cap, size_t *pos, uint8_t major, uint64_t value)
{
    if (*pos > cap) return false;
    size_t n = value < 24 ? 1 : value <= UINT8_MAX ? 2 : value <= UINT16_MAX ? 3 : value <= UINT32_MAX ? 5 : 9;
    if (n > cap - *pos) return false;
    if (n == 1) { out[(*pos)++] = (major << 5) | (uint8_t)value; return true; }
    uint8_t ai = n == 2 ? 24 : n == 3 ? 25 : n == 5 ? 26 : 27;
    out[(*pos)++] = (major << 5) | ai;
    for (size_t i = n - 1; i > 0; --i) out[(*pos)++] = (uint8_t)(value >> ((i - 1) * 8));
    return true;
}

static bool put_uint(uint8_t *out, size_t cap, size_t *pos, uint64_t value)
{
    return put_head(out, cap, pos, 0, value);
}

static bool put_bytes(uint8_t *out, size_t cap, size_t *pos, const uint8_t *bytes, size_t len)
{
    if (!put_head(out, cap, pos, 2, len) || len > cap - *pos) return false;
    memcpy(out + *pos, bytes, len);
    *pos += len;
    return true;
}

static bool put_text(uint8_t *out, size_t cap, size_t *pos, const char *text)
{
    size_t len = strlen(text);
    if (!put_head(out, cap, pos, 3, len) || len > cap - *pos) return false;
    memcpy(out + *pos, text, len);
    *pos += len;
    return true;
}

static bool put_bool(uint8_t *out, size_t cap, size_t *pos, bool value)
{
    if (*pos >= cap) return false;
    out[(*pos)++] = value ? 0xf5 : 0xf4;
    return true;
}

static size_t begin_response(const admin_request_t *request, uint64_t code, bool result, size_t *pos)
{
    *pos = 0;
    if (!put_head(s_response, sizeof(s_response), pos, 5, result ? 6 : 5) ||
        !put_uint(s_response, sizeof(s_response), pos, 0) || !put_uint(s_response, sizeof(s_response), pos, request->version_major) ||
        !put_uint(s_response, sizeof(s_response), pos, 1) || !put_uint(s_response, sizeof(s_response), pos, request->version_minor) ||
        !put_uint(s_response, sizeof(s_response), pos, 2) || !put_uint(s_response, sizeof(s_response), pos, request->command) ||
        !put_uint(s_response, sizeof(s_response), pos, 3) || !put_bytes(s_response, sizeof(s_response), pos, request->request_id, 16) ||
        !put_uint(s_response, sizeof(s_response), pos, 4) || !put_uint(s_response, sizeof(s_response), pos, code)) return 0;
    if (result && !put_uint(s_response, sizeof(s_response), pos, 5)) return 0;
    return *pos;
}

static size_t encode_empty_result(const admin_request_t *request)
{
    size_t pos;
    if (!begin_response(request, 0, true, &pos) || !put_head(s_response, sizeof(s_response), &pos, 5, 0)) return 0;
    return pos;
}

static size_t encode_error(const admin_request_t *request, uint64_t code)
{
    size_t pos;
    return begin_response(request, code, false, &pos) ? pos : 0;
}

static size_t encode_status_response(const admin_request_t *request)
{
    uint8_t fingerprint[32], tls_hash[32];
    uint8_t led_brightness, led_rgb[3];
    bool led_supported, led_enabled;
    size_t spki_len = 0, pos;
    if (device_identity_status(fingerprint, tls_hash, s_spki, &spki_len) != ESP_OK ||
        led_control_get_preferences(&led_supported, &led_enabled, &led_brightness, led_rgb) != ESP_OK ||
        !begin_response(request, 0, true, &pos) || !put_head(s_response, sizeof(s_response), &pos, 5, 6) ||
        !put_uint(s_response, sizeof(s_response), &pos, 0) || !put_bytes(s_response, sizeof(s_response), &pos, fingerprint, 32) ||
        !put_uint(s_response, sizeof(s_response), &pos, 1) || !put_bytes(s_response, sizeof(s_response), &pos, tls_hash, 32) ||
        !put_uint(s_response, sizeof(s_response), &pos, 2) || !put_uint(s_response, sizeof(s_response), &pos, wifi_setup_state()) ||
        !put_uint(s_response, sizeof(s_response), &pos, 3) ||
        !put_text(s_response, sizeof(s_response), &pos, esp_app_get_description()->version) ||
        !put_uint(s_response, sizeof(s_response), &pos, 4) || !put_bytes(s_response, sizeof(s_response), &pos, s_spki, spki_len) ||
        !put_uint(s_response, sizeof(s_response), &pos, 5) || !put_head(s_response, sizeof(s_response), &pos, 5, 4) ||
        !put_uint(s_response, sizeof(s_response), &pos, 0) || !put_bool(s_response, sizeof(s_response), &pos, led_supported) ||
        !put_uint(s_response, sizeof(s_response), &pos, 1) || !put_bool(s_response, sizeof(s_response), &pos, led_enabled) ||
        !put_uint(s_response, sizeof(s_response), &pos, 2) || !put_uint(s_response, sizeof(s_response), &pos, led_brightness) ||
        !put_uint(s_response, sizeof(s_response), &pos, 3) || !put_bytes(s_response, sizeof(s_response), &pos, led_rgb, sizeof(led_rgb))) return 0;
    return pos;
}

static size_t dispatch_request(admin_request_t *request)
{
    if (request->command == ADMIN_STATUS) return encode_status_response(request);
    if (request->command == ADMIN_WIFI_SET) {
        esp_err_t err = wifi_setup_provision(request->ssid, request->ssid_len,
                                             request->passphrase, request->passphrase_len);
        mbedtls_platform_zeroize(request->passphrase, sizeof(request->passphrase));
        return err == ESP_OK ? encode_empty_result(request) : encode_error(request, err == ESP_ERR_INVALID_ARG ? 1 : 6);
    }
    if (request->command == ADMIN_LED_SET) {
        esp_err_t err = led_control_set(request->led_enabled, request->led_brightness, request->led_rgb);
        if (err == ESP_OK) return encode_empty_result(request);
        return encode_error(request, err == ESP_ERR_NOT_SUPPORTED ? 7 : err == ESP_ERR_INVALID_ARG ? 1 : 6);
    }
    return encode_error(request, 7);
}

static bool write_frame(const uint8_t *payload, size_t len)
{
    uint8_t prefix[4] = { (uint8_t)(len >> 24), (uint8_t)(len >> 16), (uint8_t)(len >> 8), (uint8_t)len };
    return uart_write_bytes(ADMIN_UART, prefix, sizeof(prefix)) == sizeof(prefix) &&
           uart_write_bytes(ADMIN_UART, payload, len) == (int)len &&
           uart_wait_tx_done(ADMIN_UART, pdMS_TO_TICKS(1000)) == ESP_OK;
}

static bool read_exact(uint8_t *out, size_t len)
{
    size_t pos = 0;
    while (pos < len) {
        int read = uart_read_bytes(ADMIN_UART, out + pos, len - pos, pdMS_TO_TICKS(1000));
        if (read <= 0) return false;
        pos += (size_t)read;
    }
    return true;
}

static void admin_task(void *arg)
{
    (void)arg;
    for (;;) {
        uint8_t prefix[4];
        if (!read_exact(prefix, sizeof(prefix))) {
            uart_flush_input(ADMIN_UART);
            continue;
        }
        size_t len = ((size_t)prefix[0] << 24) | ((size_t)prefix[1] << 16) |
                     ((size_t)prefix[2] << 8) | prefix[3];
        if (len == 0 || len > sizeof(s_request)) {
            uart_flush_input(ADMIN_UART);
            continue;
        }
        if (!read_exact(s_request, len)) {
            uart_flush_input(ADMIN_UART);
            mbedtls_platform_zeroize(s_request, sizeof(s_request));
            continue;
        }
        admin_request_t request = {0};
        request_decode_result_t decoded = decode_request(s_request, len, &request);
        if (decoded != REQUEST_INVALID) {
            size_t response_len = decoded == REQUEST_UNSUPPORTED_VERSION
                ? encode_error(&request, 4)
                : dispatch_request(&request);
            if (response_len != 0) write_frame(s_response, response_len);
        }
        mbedtls_platform_zeroize(request.passphrase, sizeof(request.passphrase));
        mbedtls_platform_zeroize(s_request, len);
    }
}

esp_err_t usb_admin_start(void)
{
    esp_err_t err = uart_driver_install(ADMIN_UART, 4096, 4096, 0, NULL, 0);
    if (err != ESP_OK) return err;
    usb_admin_silence_logs();
    if (xTaskCreate(admin_task, "usb_admin", 6144, NULL, 5, NULL) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}
