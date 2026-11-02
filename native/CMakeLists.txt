cmake_minimum_required(VERSION 3.24)
project(RemoteMediaAudioCpp LANGUAGES C CXX)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(AUDIOCPP_BUILD_C_API ON CACHE BOOL "" FORCE)
set(AUDIOCPP_BUILD_NATIVE_MODEL_MANAGER ON CACHE BOOL "" FORCE)
set(AUDIOCPP_DEPLOYMENT_BUILD ON CACHE BOOL "" FORCE)
set(ENGINE_ENABLE_NATIVE_CPU OFF CACHE BOOL "" FORCE)
set(AUDIOCPP_VERSION "06c13737cc39481787a03465697b58b1c954b56f" CACHE STRING "" FORCE)
if(NOT AUDIOCPP_SOURCE_DIR)
  include(FetchContent)
  FetchContent_Declare(audiocpp_source
    GIT_REPOSITORY https://github.com/0xShug0/audio.cpp
    GIT_TAG 06c13737cc39481787a03465697b58b1c954b56f
    GIT_SUBMODULES "")
  FetchContent_GetProperties(audiocpp_source)
  if(NOT audiocpp_source_POPULATED)
    FetchContent_Populate(audiocpp_source)
  endif()
  set(AUDIOCPP_SOURCE_DIR "${audiocpp_source_SOURCE_DIR}")
endif()
file(SHA256 "${CMAKE_CURRENT_SOURCE_DIR}/audiocpp.h" EXPECTED_HEADER)
file(SHA256 "${AUDIOCPP_SOURCE_DIR}/include/audiocpp.h" ACTUAL_HEADER)
if(NOT EXPECTED_HEADER STREQUAL ACTUAL_HEADER)
  message(FATAL_ERROR "audio.cpp header differs from the pinned Rust bindings header")
endif()
add_subdirectory("${AUDIOCPP_SOURCE_DIR}" audiocpp-source EXCLUDE_FROM_ALL)
add_library(audiocpp_manager_bridge SHARED manager.cpp)
target_link_libraries(audiocpp_manager_bridge PRIVATE audiocpp_package_manager)
set_target_properties(audiocpp_manager_bridge PROPERTIES
  RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
  LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
  BUILD_RPATH "$ORIGIN")
add_custom_target(remotemedia_audiocpp_native DEPENDS audiocpp audiocpp_manager_bridge)
