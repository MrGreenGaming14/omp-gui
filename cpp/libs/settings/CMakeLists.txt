find_package(tomlplusplus REQUIRED)
find_package(yaml-cpp REQUIRED)

add_library(loa_settings STATIC
  src/codec.cpp
  src/error.cpp
  src/projects.cpp
  src/schema.cpp
  src/store.cpp
  src/toml.cpp
  src/updates.cpp
  src/validate.cpp
  src/models/agents.cpp
  src/models/config.cpp
  src/models/error.cpp
  src/models/keys.cpp
  src/models/models_yml.cpp
  src/models/rpc.cpp
  src/models/selector.cpp
  src/models/settings.cpp
  src/models/workflows.cpp
  src/models/yaml.cpp)
add_library(loa::settings ALIAS loa_settings)
target_include_directories(loa_settings PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_settings PUBLIC loa::contracts loa::core loa::runtime PRIVATE tomlplusplus::tomlplusplus yaml-cpp::yaml-cpp)

loa_add_test(settings
  SOURCES
    tests/config_test.cpp
    tests/updates_test.cpp
    tests/models_agents_test.cpp
    tests/models_config_test.cpp
    tests/models_keys_test.cpp
    tests/models_rpc_test.cpp
    tests/models_settings_test.cpp
    tests/models_yaml_test.cpp
    tests/models_workflows_test.cpp
    tests/models_yml_test.cpp
  LINK loa::settings)
target_compile_definitions(loa_settings_tests PRIVATE
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden"
  LOA_SETTINGS_FIXTURES="${CMAKE_CURRENT_SOURCE_DIR}/tests/fixtures")
