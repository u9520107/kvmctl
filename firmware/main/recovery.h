#pragma once

#include "esp_err.h"

/* BOOT is the ESP32-S3 ROM/app button: active low on GPIO0. */
esp_err_t recovery_start(void);
