idf_component_register(
    SRCS "tusb_hid_example_main.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES heap esp_wifi esp_event esp_netif mdns
)
