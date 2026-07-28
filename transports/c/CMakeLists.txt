cmake_minimum_required(VERSION 3.18)            # SOURCE_SUBDIR + FetchContent_MakeAvailable
project(csil_transport C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)                      # strict -std=c11, no GNU extensions

add_library(csil_transport
    src/cbor.c
    src/conventions.c
    src/carrier.c
    src/rpc.c
    src/events.c
    src/datagrams.c)
# Namespaced ALIAS so add_subdirectory and a future find_package link the same name.
add_library(csil::transport ALIAS csil_transport)

target_include_directories(csil_transport PUBLIC
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<INSTALL_INTERFACE:include>)
# src/ headers (cbor_internal.h, envelope_internal.h) are NOT on the public path.
target_include_directories(csil_transport PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/src)

if(MSVC)
    target_compile_options(csil_transport PRIVATE /W4)
else()
    target_compile_options(csil_transport PRIVATE -Wall -Wextra -Wpedantic)
endif()

# PROJECT_IS_TOP_LEVEL (CMake >= 3.21) guards tests so an add_subdirectory consumer
# does not build them; older CMake falls back to the equality idiom.
if(NOT DEFINED PROJECT_IS_TOP_LEVEL)
    if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
        set(PROJECT_IS_TOP_LEVEL TRUE)
    else()
        set(PROJECT_IS_TOP_LEVEL FALSE)
    endif()
endif()

option(BUILD_TESTING "Build the csil_transport tests" ${PROJECT_IS_TOP_LEVEL})

if(PROJECT_IS_TOP_LEVEL AND BUILD_TESTING)
    enable_testing()

    add_executable(test_conformance tests/test_conformance.c)
    target_link_libraries(test_conformance PRIVATE csil_transport)
    target_include_directories(test_conformance PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/tests)
    add_test(NAME conformance COMMAND test_conformance)

    add_executable(test_roundtrip tests/test_roundtrip.c)
    target_link_libraries(test_roundtrip PRIVATE csil_transport)
    add_test(NAME roundtrip COMMAND test_roundtrip)

    add_executable(test_max_frame tests/test_max_frame.c)
    target_link_libraries(test_max_frame PRIVATE csil_transport)
    add_test(NAME max_frame COMMAND test_max_frame)

    if(NOT MSVC)
        target_compile_options(test_conformance PRIVATE -Wall -Wextra -Wpedantic)
        target_compile_options(test_roundtrip PRIVATE -Wall -Wextra -Wpedantic)
        target_compile_options(test_max_frame PRIVATE -Wall -Wextra -Wpedantic)
    endif()
endif()
