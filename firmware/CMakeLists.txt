cmake_minimum_required(VERSION 3.16)

if("$ENV{KVMCTL_HID_BENCH}" STREQUAL "1")
    list(APPEND EXTRA_COMPONENT_DIRS "${CMAKE_CURRENT_LIST_DIR}/hid_bench")
    list(APPEND SDKCONFIG_DEFAULTS "sdkconfig.defaults" "hid_bench/sdkconfig.defaults")
    if(DEFINED ENV{KVMCTL_HID_BENCH_COMPONENTS})
        list(APPEND EXTRA_COMPONENT_DIRS
            "$ENV{KVMCTL_HID_BENCH_COMPONENTS}/espressif__esp_tinyusb"
            "$ENV{KVMCTL_HID_BENCH_COMPONENTS}/espressif__tinyusb")
    endif()
endif()

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
idf_build_set_property(MINIMAL_BUILD ON)
if(KVMCTL_DEPENDENCIES_LOCK)
    idf_build_set_property(DEPENDENCIES_LOCK "${KVMCTL_DEPENDENCIES_LOCK}")
endif()
project(kvmctl_firmware)
