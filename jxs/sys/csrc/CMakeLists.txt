# Issue 1387 -- builds the vendored SVT-JPEG-XS library (vendor/svt-jpeg-xs, pinned commit in
# vendor/svt-jpeg-xs/CAMERA-BOX-VENDOR.md) as a static library plus the camera-box shim, both
# into the build dir. build.rs drives it; nothing is written into the vendored source tree.
cmake_minimum_required(VERSION 3.10)
project(camera_box_jxs C)

if(NOT DEFINED SVT_JXS_SOURCE_DIR)
    message(FATAL_ERROR "SVT_JXS_SOURCE_DIR must point at vendor/svt-jpeg-xs")
endif()

# The upstream defaults would build shared libraries, the apps, and write the outputs into
# <source>/Bin. All three are overridden here (cache values win over the upstream set(... CACHE)).
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(BUILD_APPS OFF CACHE BOOL "" FORCE)
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
# Rust links the archive with the system cc; GCC LTO objects would need the LTO plugin at that link.
set(JPEGXS_LTO OFF CACHE BOOL "" FORCE)
# Never -march=native: the archive built on a CI runner runs on the camboxes (N100/N150), and the
# library picks its SIMD path at run time (cpuinfo).
set(NATIVE OFF CACHE BOOL "" FORCE)
set(CMAKE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib" CACHE PATH "" FORCE)

add_subdirectory("${SVT_JXS_SOURCE_DIR}" svt-jpeg-xs)

add_library(cbjxs_shim STATIC jxs_shim.c)
target_include_directories(cbjxs_shim PRIVATE "${SVT_JXS_SOURCE_DIR}/Source/API")
set_target_properties(cbjxs_shim PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
    POSITION_INDEPENDENT_CODE ON)
