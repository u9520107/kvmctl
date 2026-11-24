#include "recovery.h"

#include "driver/gpio.h"
#include "esp_log.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs.h"
#include "transport.h"
#include <stddef.h>
#include <stdbool.h>

#define BOOT_GPIO GPIO_NUM_0
#define BOOT_HOLD_US (10LL * 1000LL * 1000LL)
#define BOOT_POLL_MS 50

static const char *TAG = "kvmctl_recovery";

static const char *const kvmctl_namespaces[] = {
    "kvmctl_wifi",
    "kvmctl_id",
    "kvmctl_auth",
    "kvmctl_cfg",
};

static esp_err_t erase_namespace(const char *name)
{
    nvs_handle_t nvs;
    esp_err_t err = nvs_open(name, NVS_READWRITE, &nvs);
    if (err == ESP_ERR_NVS_NOT_FOUND) return ESP_OK;
    if (err != ESP_OK) return err;
    err = nvs_erase_all(nvs);
    if (err == ESP_OK) err = nvs_commit(nvs);
    nvs_close(nvs);
    return err;
}

static esp_err_t erase_kvmctl_storage(void)
{
    for (size_t i = 0; i < sizeof(kvmctl_namespaces) / sizeof(kvmctl_namespaces[0]); ++i) {
        esp_err_t err = erase_namespace(kvmctl_namespaces[i]);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "factory reset erase failed namespace=%s err=%s",
                     kvmctl_namespaces[i], esp_err_to_name(err));
            return err;
        }
    }
    return ESP_OK;
}

static void recovery_task(void *arg)
{
    (void)arg;
    int64_t pressed_since = 0;
    for (;;) {
        if (gpio_get_level(BOOT_GPIO) == 0) {
            if (pressed_since == 0) pressed_since = esp_timer_get_time();
            if (esp_timer_get_time() - pressed_since >= BOOT_HOLD_US) {
                ESP_LOGW(TAG, "factory reset requested after continuous BOOT hold");
                (void)transport_close();
                esp_err_t err = erase_kvmctl_storage();
                if (err == ESP_OK) {
                    ESP_LOGW(TAG, "factory reset complete; rebooting with fresh identity");
                    esp_restart();
                }
                /* Allow a released-and-reheld button to retry after an erase error. */
                pressed_since = 0;
            }
        } else {
            pressed_since = 0;
        }
        vTaskDelay(pdMS_TO_TICKS(BOOT_POLL_MS));
    }
}

esp_err_t recovery_start(void)
{
    const gpio_config_t config = {
        .pin_bit_mask = 1ULL << BOOT_GPIO,
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&config);
    if (err != ESP_OK) return err;
    return xTaskCreate(recovery_task, "kvmctl_recovery", 3072, NULL, 6, NULL) == pdPASS
        ? ESP_OK : ESP_ERR_NO_MEM;
}
