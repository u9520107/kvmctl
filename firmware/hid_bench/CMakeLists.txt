idf_component_register(SRCS "hid_bench.c"
    INCLUDE_DIRS "."
    PRIV_INCLUDE_DIRS "../main")

if("$ENV{KVMCTL_HID_BENCH_FAIL_TRANSFER_ONCE}" STREQUAL "1")
    target_compile_definitions(${COMPONENT_LIB} PRIVATE KVMCTL_HID_BENCH_FAIL_TRANSFER_ONCE=1)
endif()
