cmake_minimum_required(VERSION 3.16)

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
idf_build_set_property(MINIMAL_BUILD ON)
if(KVMCTL_DEPENDENCIES_LOCK)
    idf_build_set_property(DEPENDENCIES_LOCK "${KVMCTL_DEPENDENCIES_LOCK}")
endif()
project(kvmctl_firmware)
