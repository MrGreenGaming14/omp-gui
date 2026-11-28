# loa::iroh_ffi — RAII wrappers over the vendored iroh-c-ffi fork (fork/).
# The fork is prebuilt with cargo into a target dir shared across worktrees,
# so unchanged rebuilds reuse compiled iroh.
include(ExternalProject)

find_program(LOA_CARGO cargo REQUIRED)

if(DEFINED ENV{LOA_RUST_FFI_TARGET} AND NOT "$ENV{LOA_RUST_FFI_TARGET}" STREQUAL "")
  set(LOA_RUST_FFI_TARGET "$ENV{LOA_RUST_FFI_TARGET}" CACHE PATH "Shared cargo target dir for Rust FFI forks")
else()
  set(LOA_RUST_FFI_TARGET "$ENV{HOME}/.cache/loa/rust-ffi-target" CACHE PATH "Shared cargo target dir for Rust FFI forks")
endif()

set(_iroh_fork "${CMAKE_CURRENT_SOURCE_DIR}/fork")
set(_iroh_lib "${LOA_RUST_FFI_TARGET}/release/${CMAKE_STATIC_LIBRARY_PREFIX}irohnet${CMAKE_STATIC_LIBRARY_SUFFIX}")

# cargo does its own up-to-date check; BUILD_ALWAYS lets it decide.
ExternalProject_Add(loa_irohnet_cargo
  SOURCE_DIR "${_iroh_fork}"
  CONFIGURE_COMMAND ""
  BUILD_COMMAND ${CMAKE_COMMAND} -E env CARGO_TARGET_DIR=${LOA_RUST_FFI_TARGET}
    ${LOA_CARGO} build --release --locked --lib --manifest-path "${_iroh_fork}/Cargo.toml"
  BUILD_IN_SOURCE TRUE
  BUILD_ALWAYS TRUE
  INSTALL_COMMAND ""
  BUILD_BYPRODUCTS "${_iroh_lib}"
  USES_TERMINAL_BUILD TRUE)

add_library(loa_irohnet STATIC IMPORTED GLOBAL)
set_target_properties(loa_irohnet PROPERTIES
  IMPORTED_LOCATION "${_iroh_lib}"
  INTERFACE_INCLUDE_DIRECTORIES "${_iroh_fork}")
find_package(Threads REQUIRED)
target_link_libraries(loa_irohnet INTERFACE Threads::Threads ${CMAKE_DL_LIBS} m)
add_dependencies(loa_irohnet loa_irohnet_cargo)

add_library(loa_iroh_ffi STATIC src/iroh.cpp)
add_library(loa::iroh_ffi ALIAS loa_iroh_ffi)
target_include_directories(loa_iroh_ffi PUBLIC include)
target_link_libraries(loa_iroh_ffi PUBLIC loa_irohnet)

loa_add_test(iroh_ffi SOURCES tests/iroh_test.cpp LINK loa::iroh_ffi)
