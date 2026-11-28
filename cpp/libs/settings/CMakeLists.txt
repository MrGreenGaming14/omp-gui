find_package(tomlplusplus REQUIRED)

add_library(loa_settings STATIC
  src/codec.cpp
  src/error.cpp
  src/projects.cpp
  src/schema.cpp
  src/store.cpp
  src/toml.cpp
  src/updates.cpp
  src/validate.cpp)
add_library(loa::settings ALIAS loa_settings)
target_include_directories(loa_settings PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_settings PUBLIC loa::contracts loa::core loa::runtime PRIVATE tomlplusplus::tomlplusplus)

loa_add_test(settings SOURCES tests/config_test.cpp tests/updates_test.cpp LINK loa::settings)
target_compile_definitions(loa_settings_tests PRIVATE
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden"
  LOA_SETTINGS_FIXTURES="${CMAKE_CURRENT_SOURCE_DIR}/tests/fixtures")
