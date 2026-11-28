# loa_biscuit_ffi: C++ wrapper over the biscuit-capi fork in fork/ (biscuit-auth
# 6.0), prebuilt by cargo into the shared Rust FFI target directory.
include(ExternalProject)

find_program(LOA_CARGO cargo REQUIRED)
if(DEFINED ENV{LOA_RUST_FFI_TARGET})
  set(_loa_rust_target "$ENV{LOA_RUST_FFI_TARGET}")
else()
  set(_loa_rust_target "$ENV{HOME}/.cache/loa/rust-ffi-target")
endif()
set(LOA_RUST_FFI_TARGET "${_loa_rust_target}" CACHE PATH "Cargo target directory shared by the Rust FFI forks")

set(_fork "${CMAKE_CURRENT_SOURCE_DIR}/fork")
set(_biscuit_capi_lib "${LOA_RUST_FFI_TARGET}/release/${CMAKE_STATIC_LIBRARY_PREFIX}biscuit_capi${CMAKE_STATIC_LIBRARY_SUFFIX}")

# Cargo tracks its own inputs, so it runs on every build and is a no-op when
# nothing changed.
ExternalProject_Add(loa_biscuit_capi_cargo
  SOURCE_DIR "${_fork}"
  CONFIGURE_COMMAND ""
  BUILD_COMMAND "${LOA_CARGO}" build --release --locked
                --manifest-path "${_fork}/Cargo.toml"
                --target-dir "${LOA_RUST_FFI_TARGET}"
  BUILD_IN_SOURCE TRUE
  BUILD_ALWAYS TRUE
  INSTALL_COMMAND ""
  BUILD_BYPRODUCTS "${_biscuit_capi_lib}"
  USES_TERMINAL_BUILD TRUE)

add_library(loa_biscuit_capi STATIC IMPORTED GLOBAL)
set_target_properties(loa_biscuit_capi PROPERTIES IMPORTED_LOCATION "${_biscuit_capi_lib}")
find_package(Threads REQUIRED)
target_link_libraries(loa_biscuit_capi INTERFACE Threads::Threads ${CMAKE_DL_LIBS} m)
add_dependencies(loa_biscuit_capi loa_biscuit_capi_cargo)

add_library(loa_biscuit_ffi STATIC src/biscuit.cpp)
add_library(loa::biscuit_ffi ALIAS loa_biscuit_ffi)
target_include_directories(loa_biscuit_ffi PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_biscuit_ffi PUBLIC loa_biscuit_capi)
target_compile_features(loa_biscuit_ffi PUBLIC cxx_std_23)

loa_add_test(biscuit_ffi SOURCES tests/biscuit_ffi_tests.cpp LINK loa::biscuit_ffi)
target_compile_definitions(loa_biscuit_ffi_tests PRIVATE
  LOA_BISCUIT_FIXTURE_DIR="${PROJECT_SOURCE_DIR}/tests/golden/trust")
