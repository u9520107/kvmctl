#pragma once

#include "esp_err.h"
#include <stdbool.h>
#include <stdint.h>

esp_err_t led_control_init_off(void);
esp_err_t led_control_load_preferences(void);
esp_err_t led_control_set(bool enabled, uint8_t brightness, const uint8_t rgb[3]);
esp_err_t led_control_get_preferences(bool *supported, bool *enabled, uint8_t *brightness, uint8_t rgb[3]);
bool led_control_supported(void);
