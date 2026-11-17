idf_component_register(
    SRCS "app_main.c" "device_identity.c" "led_control.c" "transport.c" "usb_admin.c" "wifi_setup.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES esp_app_format esp_driver_gpio esp_driver_uart esp_event esp_netif esp_partition esp_peer esp_psram esp_timer esp_wifi spi_flash led_strip log mbedtls nvs_flash
)
