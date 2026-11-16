/*
 * SPDX-FileCopyrightText: 2022-2025 Espressif Systems (Shanghai) CO LTD
 * SPDX-License-Identifier: Unlicense OR CC0-1.0
 *
 * M0 probe: HID enumeration, UART0 status, and RAM-only Wi-Fi/mDNS check.
 * No input capture or HID reports. Credentials arrive only over CH343 UART0.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>

#include "esp_event.h"
#include "esp_heap_caps.h"
#include "esp_netif.h"
#include "esp_log.h"
#include "esp_wifi.h"
#include "mdns.h"

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "tinyusb.h"
#include "tinyusb_default_config.h"
#include "class/hid/hid_device.h"

#define UART_LINE_CAPACITY 256

typedef enum {
    WIFI_STATE_DISABLED,
    WIFI_STATE_CONNECTING,
    WIFI_STATE_CONNECTED,
    WIFI_STATE_FAILED,
} wifi_state_t;

static portMUX_TYPE s_wifi_state_mux = portMUX_INITIALIZER_UNLOCKED;
static wifi_state_t s_wifi_state = WIFI_STATE_DISABLED;
static esp_ip4_addr_t s_wifi_ip;
static uint8_t s_wifi_disconnect_reason;
static bool s_mdns_ready;
static bool s_wifi_initialized;
static bool s_wifi_connect_attempted;

static const char *TAG = "m0_probe";

#define TUSB_DESC_TOTAL_LEN (TUD_CONFIG_DESC_LEN + CFG_TUD_HID * TUD_HID_DESC_LEN)

const uint8_t hid_report_descriptor[] = {
    TUD_HID_REPORT_DESC_KEYBOARD(HID_REPORT_ID(HID_ITF_PROTOCOL_KEYBOARD)),
    TUD_HID_REPORT_DESC_MOUSE(HID_REPORT_ID(HID_ITF_PROTOCOL_MOUSE))
};

const char *hid_string_descriptor[5] = {
    (char[]){0x09, 0x04},
    "TinyUSB",
    "TinyUSB Device",
    "123456",
    "Example HID interface",
};

static const uint8_t hid_configuration_descriptor[] = {
    TUD_CONFIG_DESCRIPTOR(1, 1, 0, TUSB_DESC_TOTAL_LEN, TUSB_DESC_CONFIG_ATT_REMOTE_WAKEUP, 100),
    TUD_HID_DESCRIPTOR(0, 4, false, sizeof(hid_report_descriptor), 0x81, 16, 10),
};

uint8_t const *tud_hid_descriptor_report_cb(uint8_t instance)
{
    (void)instance;
    return hid_report_descriptor;
}

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id,
                               hid_report_type_t report_type, uint8_t *buffer,
                               uint16_t requested_len)
{
    (void)instance;
    (void)report_id;
    (void)report_type;
    (void)buffer;
    (void)requested_len;
    return 0;
}

void tud_hid_set_report_cb(uint8_t instance, uint8_t report_id,
                           hid_report_type_t report_type,
                           uint8_t const *buffer, uint16_t buffer_size)
{
    (void)instance;
    (void)report_id;
    (void)report_type;
    (void)buffer;
    (void)buffer_size;
}

static void log_heap(const char *phase)
{
    ESP_LOGI(TAG, "heap phase=%s internal_free=%u internal_min=%u psram_free=%u psram_total=%u",
             phase,
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
             (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM),
             (unsigned)heap_caps_get_total_size(MALLOC_CAP_SPIRAM));
}

static const char *wifi_state_name(wifi_state_t state)
{
    switch (state) {
    case WIFI_STATE_CONNECTING: return "connecting";
    case WIFI_STATE_CONNECTED: return "connected";
    case WIFI_STATE_FAILED: return "failed";
    default: return "disabled";
    }
}

static void start_mdns(void)
{
    if (mdns_init() != ESP_OK) {
        return;
    }
    if (mdns_hostname_set("kvmctl-m0-probe") != ESP_OK) {
        mdns_free();
        return;
    }

    portENTER_CRITICAL(&s_wifi_state_mux);
    s_mdns_ready = true;
    portEXIT_CRITICAL(&s_wifi_state_mux);
}

static void wifi_event_handler(void *arg, esp_event_base_t event_base,
                               int32_t event_id, void *event_data)
{
    (void)arg;

    if (event_base == IP_EVENT && event_id == IP_EVENT_STA_GOT_IP) {
        ip_event_got_ip_t *event = (ip_event_got_ip_t *)event_data;
        start_mdns();
        portENTER_CRITICAL(&s_wifi_state_mux);
        s_wifi_ip = event->ip_info.ip;
        s_wifi_disconnect_reason = 0;
        s_wifi_state = WIFI_STATE_CONNECTED;
        portEXIT_CRITICAL(&s_wifi_state_mux);
    } else if (event_base == WIFI_EVENT && event_id == WIFI_EVENT_STA_DISCONNECTED) {
        wifi_event_sta_disconnected_t *event = (wifi_event_sta_disconnected_t *)event_data;
        bool free_mdns;

        portENTER_CRITICAL(&s_wifi_state_mux);
        free_mdns = s_mdns_ready;
        s_mdns_ready = false;
        s_wifi_ip.addr = 0;
        s_wifi_disconnect_reason = event->reason;
        s_wifi_state = WIFI_STATE_FAILED;
        portEXIT_CRITICAL(&s_wifi_state_mux);

        if (free_mdns) {
            mdns_free();
        }
    }
}

static bool decode_hex(const char *text, size_t max_bytes, uint8_t *output,
                       size_t *output_len)
{
    size_t len = strlen(text);
    if ((len & 1) != 0 || len / 2 > max_bytes) {
        return false;
    }

    for (size_t i = 0; i < len; i += 2) {
        char pair[3] = {text[i], text[i + 1], '\0'};
        char *end = NULL;
        unsigned long value = strtoul(pair, &end, 16);
        if (end != pair + 2) {
            return false;
        }
        output[i / 2] = (uint8_t)value;
    }
    *output_len = len / 2;
    return true;
}

static bool wifi_start_station(void)
{
    if (s_wifi_initialized) {
        return true;
    }

    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    if (esp_netif_create_default_wifi_sta() == NULL) {
        return false;
    }

    wifi_init_config_t init_config = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&init_config));
    ESP_ERROR_CHECK(esp_wifi_set_storage(WIFI_STORAGE_RAM));
    ESP_ERROR_CHECK(esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID,
                                               wifi_event_handler, NULL));
    ESP_ERROR_CHECK(esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP,
                                               wifi_event_handler, NULL));
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_start());
    s_wifi_initialized = true;
    return true;
}

static const char *wifi_auth_name(wifi_auth_mode_t auth)
{
    switch (auth) {
    case WIFI_AUTH_OPEN: return "open";
    case WIFI_AUTH_WEP: return "wep";
    case WIFI_AUTH_WPA_PSK: return "wpa";
    case WIFI_AUTH_WPA2_PSK: return "wpa2";
    case WIFI_AUTH_WPA_WPA2_PSK: return "wpa-wpa2";
    case WIFI_AUTH_ENTERPRISE: return "enterprise";
    case WIFI_AUTH_WPA3_PSK: return "wpa3";
    case WIFI_AUTH_WPA2_WPA3_PSK: return "wpa2-wpa3";
    case WIFI_AUTH_WAPI_PSK: return "wapi";
    case WIFI_AUTH_OWE: return "owe";
    case WIFI_AUTH_WPA3_ENT_192: return "wpa3-enterprise";
    case WIFI_AUTH_WPA3_ENTERPRISE: return "wpa3-enterprise";
    case WIFI_AUTH_WPA2_WPA3_ENTERPRISE: return "wpa2-wpa3-enterprise";
    case WIFI_AUTH_WPA_ENTERPRISE: return "wpa-enterprise";
    case WIFI_AUTH_UNKNOWN: return "unknown";
    default: return "other";
    }
}
static void wifi_connect_from_hex(const char *ssid_hex, const char *password_hex)
{
    uint8_t ssid[32] = {0};
    uint8_t password[64] = {0};
    size_t ssid_len = 0;
    size_t password_len = 0;

    if (!decode_hex(ssid_hex, sizeof(ssid), ssid, &ssid_len) || ssid_len == 0 ||
        (strcmp(password_hex, "-") != 0 &&
         !decode_hex(password_hex, sizeof(password), password, &password_len)) ||
        (password_len != 0 && (password_len < 8 || password_len > 64))) {
        printf("ERR credentials\r\n");
        fflush(stdout);
        return;
    }

    wifi_config_t wifi_config = {0};
    memcpy(wifi_config.sta.ssid, ssid, ssid_len);
    memcpy(wifi_config.sta.password, password, password_len);
    memset(ssid, 0, sizeof(ssid));
    memset(password, 0, sizeof(password));

    if (s_wifi_connect_attempted) {
        memset(&wifi_config, 0, sizeof(wifi_config));
        printf("ERR wifi_already_attempted; reset board to retry\r\n");
        fflush(stdout);
        return;
    }
    if (!wifi_start_station()) {
        memset(&wifi_config, 0, sizeof(wifi_config));
        printf("ERR wifi_netif\r\n");
        fflush(stdout);
        return;
    }
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &wifi_config));
    s_wifi_connect_attempted = true;
    memset(&wifi_config, 0, sizeof(wifi_config));
    portENTER_CRITICAL(&s_wifi_state_mux);
    s_wifi_state = WIFI_STATE_CONNECTING;
    s_wifi_ip.addr = 0;
    s_wifi_disconnect_reason = 0;
    portEXIT_CRITICAL(&s_wifi_state_mux);

    if (esp_wifi_connect() == ESP_OK) {
        printf("WIFI connecting\r\n");
    } else {
        portENTER_CRITICAL(&s_wifi_state_mux);
        s_wifi_state = WIFI_STATE_FAILED;
        portEXIT_CRITICAL(&s_wifi_state_mux);
        printf("ERR wifi_connect\r\n");
    }
    fflush(stdout);
}

static void wifi_scan_for_ssid_hex(const char *ssid_hex)
{
    uint8_t ssid[33] = {0};
    size_t ssid_len = 0;
    wifi_state_t state;

    if (!decode_hex(ssid_hex, sizeof(ssid) - 1, ssid, &ssid_len) || ssid_len == 0) {
        printf("M0 SCAN ERROR invalid_ssid\r\nM0 SCAN DONE\r\n");
        fflush(stdout);
        return;
    }

    portENTER_CRITICAL(&s_wifi_state_mux);
    state = s_wifi_state;
    portEXIT_CRITICAL(&s_wifi_state_mux);
    if (state == WIFI_STATE_CONNECTING || state == WIFI_STATE_CONNECTED) {
        printf("M0 SCAN ERROR wifi_busy\r\nM0 SCAN DONE\r\n");
        fflush(stdout);
        memset(ssid, 0, sizeof(ssid));
        return;
    }
    if (!wifi_start_station()) {
        printf("M0 SCAN ERROR wifi_netif\r\nM0 SCAN DONE\r\n");
        fflush(stdout);
        memset(ssid, 0, sizeof(ssid));
        return;
    }

    wifi_scan_config_t scan_config = {
        .ssid = ssid,
        .show_hidden = false,
        .scan_type = WIFI_SCAN_TYPE_ACTIVE,
    };
    esp_err_t err = esp_wifi_scan_start(&scan_config, true);
    if (err != ESP_OK) {
        printf("M0 SCAN ERROR start=%d\r\nM0 SCAN DONE\r\n", (int)err);
        fflush(stdout);
        memset(ssid, 0, sizeof(ssid));
        return;
    }

    uint16_t found = 0;
    err = esp_wifi_scan_get_ap_num(&found);
    if (err != ESP_OK) {
        esp_wifi_clear_ap_list();
        printf("M0 SCAN ERROR count=%d\r\nM0 SCAN DONE\r\n", (int)err);
        fflush(stdout);
        memset(ssid, 0, sizeof(ssid));
        return;
    }
    if (found == 0) {
        esp_wifi_clear_ap_list();
        printf("M0 SCAN no_match\r\nM0 SCAN DONE\r\n");
        fflush(stdout);
        memset(ssid, 0, sizeof(ssid));
        return;
    }

    uint16_t reported = found > 8 ? 8 : found;
    wifi_ap_record_t *records = calloc(reported, sizeof(*records));
    if (records == NULL) {
        esp_wifi_clear_ap_list();
        printf("M0 SCAN ERROR memory\r\nM0 SCAN DONE\r\n");
        fflush(stdout);
        memset(ssid, 0, sizeof(ssid));
        return;
    }

    uint16_t returned = reported;
    err = esp_wifi_scan_get_ap_records(&returned, records);
    if (err == ESP_OK) {
        for (uint16_t i = 0; i < returned; ++i) {
            printf("M0 SCAN AP channel=%u rssi=%d auth=%s\r\n",
                   records[i].primary, records[i].rssi,
                   wifi_auth_name(records[i].authmode));
        }
    }
    esp_wifi_clear_ap_list();
    free(records);
    memset(ssid, 0, sizeof(ssid));
    if (err != ESP_OK) {
        printf("M0 SCAN ERROR records=%d\r\n", (int)err);
    }
    printf("M0 SCAN count=%u\r\nM0 SCAN DONE\r\n", found);
    fflush(stdout);
}
static void print_status(void)
{
    wifi_state_t state;
    esp_ip4_addr_t ip;
    uint8_t reason;
    bool mdns_ready;

    portENTER_CRITICAL(&s_wifi_state_mux);
    state = s_wifi_state;
    ip = s_wifi_ip;
    reason = s_wifi_disconnect_reason;
    mdns_ready = s_mdns_ready;
    portEXIT_CRITICAL(&s_wifi_state_mux);

    printf("M0 STATUS wifi=%s ip=" IPSTR " mdns=%s reason=%u\r\n",
           wifi_state_name(state), IP2STR(&ip), mdns_ready ? "on" : "off", reason);
    fflush(stdout);
}

static void handle_uart_line(char *line)
{
    char *save = NULL;
    char *command = strtok_r(line, " \t", &save);

    if (command == NULL) {
        return;
    }
    if (strcmp(command, "PING") == 0) {
        printf("M0 UART0 received 4 bytes\r\n");
    } else if (strcmp(command, "STATUS") == 0) {
        print_status();
        return;
    } else if (strcmp(command, "SCAN") == 0) {
        char *ssid_hex = strtok_r(NULL, " \t", &save);
        char *extra = strtok_r(NULL, " \t", &save);
        if (ssid_hex == NULL || extra != NULL) {
            printf("M0 SCAN ERROR usage\r\nM0 SCAN DONE\r\n");
            fflush(stdout);
        } else {
            wifi_scan_for_ssid_hex(ssid_hex);
            return;
        }
    } else if (strcmp(command, "WIFI") == 0) {
        char *ssid_hex = strtok_r(NULL, " \t", &save);
        char *password_hex = strtok_r(NULL, " \t", &save);
        char *extra = strtok_r(NULL, " \t", &save);
        if (ssid_hex == NULL || password_hex == NULL || extra != NULL) {
            printf("ERR credentials\r\n");
        } else {
            wifi_connect_from_hex(ssid_hex, password_hex);
            return;
        }
    } else {
        printf("ERR command\r\n");
    }
    fflush(stdout);
    log_heap("uart-line");
}

static void run_uart0_probe(void)
{
    char line[UART_LINE_CAPACITY] = {0};

    printf("M0 UART0 probe ready\r\n");
    fflush(stdout);

    while (true) {
        if (fgets(line, sizeof(line), stdin) == NULL) {
            clearerr(stdin);
            vTaskDelay(pdMS_TO_TICKS(10));
            continue;
        }

        size_t length = strcspn(line, "\r\n");
        if (line[length] == '\0' && !feof(stdin)) {
            int ch;
            while ((ch = getchar()) != '\n' && ch != EOF) { }
            printf("ERR line_too_long\r\n");
            fflush(stdout);
            memset(line, 0, sizeof(line));
            continue;
        }
        line[length] = '\0';
        handle_uart_line(line);
        memset(line, 0, sizeof(line));
    }
}

void app_main(void)
{
    ESP_LOGI(TAG, "Installing no-report HID; physical inputs are not read and no HID reports are sent");
    tinyusb_config_t tusb_config = TINYUSB_DEFAULT_CONFIG();
    tusb_config.descriptor.device = NULL;
    tusb_config.descriptor.full_speed_config = hid_configuration_descriptor;
    tusb_config.descriptor.string = hid_string_descriptor;
    tusb_config.descriptor.string_count = sizeof(hid_string_descriptor) / sizeof(hid_string_descriptor[0]);
#if (TUD_OPT_HIGH_SPEED)
    tusb_config.descriptor.high_speed_config = hid_configuration_descriptor;
#endif
    ESP_ERROR_CHECK(tinyusb_driver_install(&tusb_config));

    ESP_LOGI(TAG, "HID active; Wi-Fi starts only after a WIFI command on CH343 UART0");
    log_heap("hid-ready");
    run_uart0_probe();
}
