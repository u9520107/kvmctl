#include "esp_err.h"
#include "esp_log.h"
#include "esp_partition.h"
#include "esp_psram.h"
#include "esp_flash.h"
#include "device_identity.h"
#include "led_control.h"
#include "usb_admin.h"
#include "transport.h"
#include "wifi_setup.h"
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

static const char *TAG = "kvmctl";

#if CONFIG_ESP_CONSOLE_UART_NUM != 0 || CONFIG_ESP_CONSOLE_UART_BAUDRATE != 115200
#error "The product USB administration link requires UART0 at 115200 baud"
#endif

static void validate_board_resources(void)
{
    uint32_t flash_bytes = 0;
    esp_err_t err = esp_flash_get_physical_size(esp_flash_default_chip, &flash_bytes);
    const size_t psram_bytes = esp_psram_get_size();
    if (err != ESP_OK || flash_bytes < 16U * 1024U * 1024U ||
        psram_bytes < 8U * 1024U * 1024U) {
        ESP_LOGE(TAG, "Unsupported memory: flash=%u PSRAM=%u err=%s",
                 (unsigned)flash_bytes, (unsigned)psram_bytes, esp_err_to_name(err));
        abort();
    }

    esp_partition_iterator_t it = esp_partition_find(
        ESP_PARTITION_TYPE_ANY, ESP_PARTITION_SUBTYPE_ANY, NULL);
    bool have_partition = false;
    for (; it != NULL; it = esp_partition_next(it)) {
        const esp_partition_t *partition = esp_partition_get(it);
        const uint64_t end = (uint64_t)partition->address + partition->size;
        if (end > flash_bytes) {
            ESP_LOGE(TAG, "Partition %s exceeds detected flash", partition->label);
            esp_partition_iterator_release(it);
            abort();
        }
        have_partition = true;
    }
    if (!have_partition) {
        ESP_LOGE(TAG, "No valid partitions found");
        abort();
    }
    esp_partition_iterator_release(it);

    ESP_LOGI(TAG, "Board resources: flash=%u PSRAM=%u UART0=115200",
             (unsigned)flash_bytes, (unsigned)psram_bytes);
}

void app_main(void)
{
    esp_err_t err = led_control_init_off();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "Could not initialize addressable RGB LED off: %s", esp_err_to_name(err));
        abort();
    }
    validate_board_resources();
    err = device_identity_init();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "Device identity initialization failed: %s", esp_err_to_name(err));
        abort();
    }
    err = led_control_load_preferences();
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "RGB preferences unavailable; continuing administration: %s", esp_err_to_name(err));
    }
    usb_admin_silence_logs();
    wifi_setup_init();
    err = transport_init();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "WebRTC transport initialization failed: %s", esp_err_to_name(err));
        abort();
    }
    err = usb_admin_start();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "USB administration initialization failed: %s", esp_err_to_name(err));
        abort();
    }
}
