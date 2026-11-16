#include "led_control.h"

#include "driver/gpio.h"
#include "led_strip.h"
#include "nvs.h"
#include <string.h>

#define LED_NAMESPACE "kvmctl_cfg"

typedef struct {
    bool enabled;
    uint8_t brightness;
    uint8_t rgb[3];
} led_preferences_t;

static led_strip_handle_t s_strip;
static bool s_supported;
static led_preferences_t s_preferences = { .enabled = false, .brightness = 25, .rgb = {255, 255, 255} };

bool led_control_supported(void)
{
    return s_supported;
}

esp_err_t led_control_get_preferences(bool *supported, bool *enabled,
                                      uint8_t *brightness, uint8_t rgb[3])
{
    if (supported == NULL || enabled == NULL || brightness == NULL || rgb == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    *supported = s_supported;
    *enabled = s_preferences.enabled;
    *brightness = s_preferences.brightness;
    memcpy(rgb, s_preferences.rgb, sizeof(s_preferences.rgb));
    return ESP_OK;
}

esp_err_t led_control_init_off(void)
{
#if CONFIG_KVMCTL_RGB_LED_GPIO48 || CONFIG_KVMCTL_RGB_LED_GPIO38
#if CONFIG_KVMCTL_RGB_LED_GPIO48
    const gpio_num_t led_gpio = GPIO_NUM_48;
#else
    const gpio_num_t led_gpio = GPIO_NUM_38;
#endif
    const led_strip_config_t strip_config = {
        .strip_gpio_num = led_gpio,
        .max_leds = 1,
        .led_model = LED_MODEL_WS2812,
        .color_component_format = LED_STRIP_COLOR_COMPONENT_FMT_GRB,
    };
    const led_strip_rmt_config_t rmt_config = {
        .clk_src = RMT_CLK_SRC_DEFAULT,
        .resolution_hz = 10 * 1000 * 1000,
    };
    esp_err_t err = led_strip_new_rmt_device(&strip_config, &rmt_config, &s_strip);
    if (err != ESP_OK) return err;
    s_supported = true;
    return led_strip_clear(s_strip);
#else
    return ESP_OK;
#endif
}

static esp_err_t apply_preferences(const led_preferences_t *preferences)
{
    if (!s_supported) return ESP_ERR_NOT_SUPPORTED;
    if (!preferences->enabled) return led_strip_clear(s_strip);
    const uint32_t red = (uint32_t)preferences->rgb[0] * preferences->brightness / 100;
    const uint32_t green = (uint32_t)preferences->rgb[1] * preferences->brightness / 100;
    const uint32_t blue = (uint32_t)preferences->rgb[2] * preferences->brightness / 100;
    esp_err_t err = led_strip_set_pixel(s_strip, 0, red, green, blue);
    return err == ESP_OK ? led_strip_refresh(s_strip) : err;
}

esp_err_t led_control_load_preferences(void)
{
    if (!s_supported) return ESP_OK;
    nvs_handle_t nvs;
    esp_err_t err = nvs_open(LED_NAMESPACE, NVS_READWRITE, &nvs);
    if (err != ESP_OK) return err;
    uint8_t enabled = 0;
    err = nvs_get_u8(nvs, "enabled", &enabled);
    if (err == ESP_OK && enabled <= 1) s_preferences.enabled = enabled == 1;
    else if (err == ESP_OK) { nvs_close(nvs); return ESP_ERR_INVALID_STATE; }
    else if (err != ESP_ERR_NVS_NOT_FOUND) { nvs_close(nvs); return err; }
    uint8_t brightness = 0;
    err = nvs_get_u8(nvs, "brightness", &brightness);
    if (err == ESP_OK && brightness <= 100) s_preferences.brightness = brightness;
    else if (err != ESP_ERR_NVS_NOT_FOUND) { nvs_close(nvs); return err == ESP_OK ? ESP_ERR_INVALID_STATE : err; }
    size_t rgb_len = sizeof(s_preferences.rgb);
    err = nvs_get_blob(nvs, "rgb", s_preferences.rgb, &rgb_len);
    if (err == ESP_OK && rgb_len != sizeof(s_preferences.rgb)) { nvs_close(nvs); return ESP_ERR_INVALID_SIZE; }
    if (err != ESP_OK && err != ESP_ERR_NVS_NOT_FOUND) { nvs_close(nvs); return err; }
    nvs_close(nvs);
    return apply_preferences(&s_preferences);
}

esp_err_t led_control_set(bool enabled, uint8_t brightness, const uint8_t rgb[3])
{
    if (!s_supported) return ESP_ERR_NOT_SUPPORTED;
    if (rgb == NULL || brightness > 100) return ESP_ERR_INVALID_ARG;
    const led_preferences_t next = { .enabled = enabled, .brightness = brightness, .rgb = {rgb[0], rgb[1], rgb[2]} };
    esp_err_t err = apply_preferences(&next);
    if (err != ESP_OK) return err;

    nvs_handle_t nvs;
    err = nvs_open(LED_NAMESPACE, NVS_READWRITE, &nvs);
    if (err == ESP_OK) {
        err = nvs_set_u8(nvs, "enabled", enabled ? 1 : 0);
        if (err == ESP_OK) err = nvs_set_u8(nvs, "brightness", brightness);
        if (err == ESP_OK) err = nvs_set_blob(nvs, "rgb", rgb, 3);
        if (err == ESP_OK) err = nvs_commit(nvs);
        nvs_close(nvs);
    }
    if (err != ESP_OK) {
        apply_preferences(&s_preferences);
        return err;
    }
    s_preferences = next;
    return ESP_OK;
}
