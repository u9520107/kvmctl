if("$ENV{KVMCTL_HID_BENCH}" STREQUAL "1")
    set(hid_bench_dependency hid_bench)
endif()

idf_component_register(
    SRCS "app_main.c" "client_auth.c" "device_identity.c" "hid_protocol.c" "lease.c" "led_control.c" "signaling.c" "signaling_protocol.c" "transport.c" "usb_admin.c" "wifi_setup.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES esp_app_format esp_driver_gpio esp_driver_uart esp_event esp_http_server esp_https_server esp_netif esp_partition esp_peer esp_psram esp_timer esp_wifi spi_flash led_strip log mdns mbedtls nvs_flash ${hid_bench_dependency}
)

if(hid_bench_dependency)
    target_compile_definitions(${COMPONENT_LIB} PRIVATE KVMCTL_HID_BENCH=1)
endif()

if("$ENV{KVMCTL_TEST_REVOKE_PERSIST_FAIL_ONCE}" STREQUAL "1")
    target_compile_definitions(${COMPONENT_LIB} PRIVATE KVMCTL_TEST_REVOKE_PERSIST_FAIL_ONCE=1)
    message(STATUS "kvmctl: test build will fail the first per-client revoke persistence attempt")
endif()
