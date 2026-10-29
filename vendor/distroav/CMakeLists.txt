cmake_minimum_required(VERSION 3.28...3.30)

include("${CMAKE_CURRENT_SOURCE_DIR}/cmake/common/bootstrap.cmake" NO_POLICY_SCOPE)

project(${_name} VERSION ${_version})

option(ENABLE_FRONTEND_API "Use obs-frontend-api for UI functionality" ON)
option(ENABLE_QT "Use Qt functionality" ON)

include(compilerconfig)
include(defaults)
include(helpers)

add_library(${CMAKE_PROJECT_NAME} MODULE)

find_package(libobs REQUIRED)
target_link_libraries(${CMAKE_PROJECT_NAME} PRIVATE OBS::libobs)

find_package(CURL REQUIRED)
target_link_libraries(${CMAKE_PROJECT_NAME} PRIVATE CURL::libcurl)

if(ENABLE_FRONTEND_API)
  find_package(obs-frontend-api REQUIRED)
  target_link_libraries(${CMAKE_PROJECT_NAME} PRIVATE OBS::obs-frontend-api)
endif()

if(ENABLE_QT)
  find_package(Qt6 COMPONENTS Widgets Core Network)
  target_link_libraries(${CMAKE_PROJECT_NAME} PRIVATE Qt6::Core Qt6::Widgets Qt6::Network)
  if(APPLE)
    foreach(
      _qt_target
      IN
      ITEMS Qt6::Core Qt6::Gui Qt6::Widgets Qt6::Network WrapOpenGL::WrapOpenGL OpenGL::GL OpenGL::GLU
    )
      if(TARGET ${_qt_target})
        foreach(_qt_prop IN ITEMS INTERFACE_LINK_LIBRARIES INTERFACE_LINK_OPTIONS)
          get_target_property(_qt_prop_value ${_qt_target} ${_qt_prop})
          if(_qt_prop_value)
            set(_qt_prop_cleaned "${_qt_prop_value}")
            string(REPLACE "-framework;AGL;" "" _qt_prop_cleaned "${_qt_prop_cleaned}")
            string(REPLACE ";-framework;AGL" "" _qt_prop_cleaned "${_qt_prop_cleaned}")
            string(REPLACE "-framework;AGL" "" _qt_prop_cleaned "${_qt_prop_cleaned}")
            string(REPLACE "-framework AGL" "" _qt_prop_cleaned "${_qt_prop_cleaned}")
            string(REPLACE "/System/Library/Frameworks/AGL.framework" "" _qt_prop_cleaned "${_qt_prop_cleaned}")
            if(NOT _qt_prop_cleaned STREQUAL _qt_prop_value)
              set_property(TARGET ${_qt_target} PROPERTY ${_qt_prop} "${_qt_prop_cleaned}")
            endif()
          endif()
        endforeach()
      endif()
    endforeach()
  endif()
  target_compile_options(
    ${CMAKE_PROJECT_NAME}
    PRIVATE $<$<C_COMPILER_ID:Clang,AppleClang>:-Wno-quoted-include-in-framework-header -Wno-comma>
  )
  set_target_properties(
    ${CMAKE_PROJECT_NAME}
    PROPERTIES AUTOMOC ON AUTOUIC ON AUTORCC ON
  )
endif()

target_sources(
  ${CMAKE_PROJECT_NAME}
  PRIVATE
    src/forms/output-settings.cpp
    src/forms/output-settings.h
    src/forms/update.cpp
    src/forms/update.h
    src/obs-support/obs-app.hpp
    src/obs-support/remote-text.cpp
    src/obs-support/remote-text.hpp
    src/config.cpp
    src/config.h
    src/main-output.cpp
    src/main-output.h
    src/ndi-filter.cpp
    src/ndi-finder.h
    src/ndi-finder.cpp
    src/ndi-output.cpp
    src/ndi-source.cpp
    # #111: QR render-time burn filter (Path B) + its vendored QR encoder (Nayuki
    # qrcodegen, MIT). Header-only burn-payload/burn-clock/burn-qr/burn-geom (the 4-corner
    # placement geometry) are pulled in by the filter TU; only the qrcodegen .cpp needs
    # compiling. No external build dependency.
    src/ndi-burn-filter.cpp
    src/qrcodegen/qrcodegen.cpp
    src/plugin-main.cpp
    src/plugin-main.h
    src/premultiplied-alpha-filter.cpp
    src/preview-output.cpp
    src/preview-output.h
)

# camera-box issue 1363: Linux-only NDI sender-port hygiene (SO_LINGER 0 on a
# sender's connections before it is destroyed, so no TIME_WAIT shifts the next
# OBS's sender ports). Windows binds over a TIME_WAIT; its source list is unchanged.
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  target_sources(
    ${CMAKE_PROJECT_NAME}
    PRIVATE
      src/ndi-sender-port.cpp
      src/ndi-sender-port.h
  )
endif()

# camera-box issue 1387 slice 2: the JPEG XS receiving source. ONLY the strih Linux build
# (linux-genlock.yml, the strih variant) turns it on, passing the jxs-recv-ffi static library cargo
# built, its header dir, and the native libraries `rustc --print native-static-libs` named for it.
# Every other build (Windows, the imag full build, the compile check) leaves it OFF: the receiver is
# Linux-only and those builds have no Rust toolchain.
option(CAMERA_BOX_JXS_SOURCE "camera-box issue 1387: build the JPEG XS source (needs jxs-recv-ffi)" OFF)
if(CAMERA_BOX_JXS_SOURCE)
  if(NOT CMAKE_SYSTEM_NAME STREQUAL "Linux")
    message(FATAL_ERROR "CAMERA_BOX_JXS_SOURCE is Linux-only (issue 1387)")
  endif()
  set(CAMERA_BOX_JXS_FFI_LIB "" CACHE FILEPATH "libjxs_recv_ffi.a (issue 1387)")
  set(CAMERA_BOX_JXS_FFI_INCLUDE "" CACHE PATH "jxs/recv-ffi/include (issue 1387)")
  set(CAMERA_BOX_JXS_FFI_NATIVE_LIBS "" CACHE STRING "rustc --print native-static-libs (issue 1387)")
  # issue 1387 slice 3: the JPEG XS program output's header (jxs/send-ffi/include). Its C ABI is
  # carried INSIDE CAMERA_BOX_JXS_FFI_LIB (one Rust archive: two would each carry std, camera-box
  # and a second SVT-JPEG-XS), so there is no second library to link.
  set(CAMERA_BOX_JXS_SEND_FFI_INCLUDE "" CACHE PATH "jxs/send-ffi/include (issue 1387 slice 3)")
  if(NOT EXISTS "${CAMERA_BOX_JXS_FFI_LIB}")
    message(FATAL_ERROR "CAMERA_BOX_JXS_FFI_LIB not found: '${CAMERA_BOX_JXS_FFI_LIB}' (issue 1387)")
  endif()
  if(NOT EXISTS "${CAMERA_BOX_JXS_FFI_INCLUDE}/camera_box_jxs.h")
    message(FATAL_ERROR "camera_box_jxs.h not in CAMERA_BOX_JXS_FFI_INCLUDE '${CAMERA_BOX_JXS_FFI_INCLUDE}' (issue 1387)")
  endif()
  if(NOT EXISTS "${CAMERA_BOX_JXS_SEND_FFI_INCLUDE}/camera_box_jxs_send.h")
    message(FATAL_ERROR "camera_box_jxs_send.h not in CAMERA_BOX_JXS_SEND_FFI_INCLUDE '${CAMERA_BOX_JXS_SEND_FFI_INCLUDE}' (issue 1387)")
  endif()
  if(CAMERA_BOX_JXS_FFI_NATIVE_LIBS STREQUAL "")
    message(FATAL_ERROR "CAMERA_BOX_JXS_FFI_NATIVE_LIBS is empty (issue 1387)")
  endif()
  target_sources(${CMAKE_PROJECT_NAME} PRIVATE src/jxs-source.cpp src/jxs-source-decide.h
                 src/jxs-output.cpp src/jxs-output-decide.h src/genlock-emit-timecode.h)
  target_compile_definitions(${CMAKE_PROJECT_NAME} PRIVATE CAMERA_BOX_JXS_SOURCE=1)
  target_include_directories(${CMAKE_PROJECT_NAME} PRIVATE "${CAMERA_BOX_JXS_FFI_INCLUDE}"
                             "${CAMERA_BOX_JXS_SEND_FFI_INCLUDE}")
  separate_arguments(_camera_box_jxs_native UNIX_COMMAND "${CAMERA_BOX_JXS_FFI_NATIVE_LIBS}")
  target_link_libraries(${CMAKE_PROJECT_NAME} PRIVATE "${CAMERA_BOX_JXS_FFI_LIB}" ${_camera_box_jxs_native})
  # -z noexecstack: SVT-JPEG-XS's nasm objects carry no .note.GNU-stack, which would mark the whole
  # plugin as needing an EXECUTABLE stack -- glibc 2.41+ (the Ubuntu 26.04 strih box) then refuses to
  # load distroav.so. --exclude-libs: no Rust or SVT symbol is exported from the plugin.
  get_filename_component(_camera_box_jxs_lib_name "${CAMERA_BOX_JXS_FFI_LIB}" NAME)
  target_link_options(${CMAKE_PROJECT_NAME} PRIVATE "LINKER:-z,noexecstack"
                      "LINKER:--exclude-libs,${_camera_box_jxs_lib_name}")
  message(STATUS "camera-box issue 1387: the JPEG XS source is built (${CAMERA_BOX_JXS_FFI_LIB})")
endif()

set(valid_uuid FALSE)
check_uuid(${WINDOWS_APP_UUID} valid_uuid)
if(NOT valid_uuid)
  message(FATAL_ERROR "Specified Windows package UUID is not a valid UUID value: ${WINDOWS_APP_UUID}")
endif()

configure_file(
  cmake/windows/resources/installer-Windows.iss.in
  "${CMAKE_CURRENT_BINARY_DIR}/installer-Windows.generated.iss"
)
file(COPY "${CMAKE_CURRENT_SOURCE_DIR}/assets/icons/distroav.ico" DESTINATION "${CMAKE_CURRENT_BINARY_DIR}")
target_include_directories(${CMAKE_PROJECT_NAME} PRIVATE ${CMAKE_SOURCE_DIR}/lib/ndi)
set_target_properties_plugin(${CMAKE_PROJECT_NAME} PROPERTIES OUTPUT_NAME ${_name})
