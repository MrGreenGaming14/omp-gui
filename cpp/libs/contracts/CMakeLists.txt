add_library(loa_contracts STATIC
  src/harness.cpp
  src/host_tools.cpp
  src/schema.cpp
  src/session.cpp
  src/surface.cpp
  src/tools.cpp)
add_library(loa::contracts ALIAS loa_contracts)
target_include_directories(loa_contracts PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_contracts PUBLIC loa::core loa::runtime)

loa_add_test(contracts
  SOURCES
    tests/contracts_test.cpp
    tests/surface_test.cpp
  LINK loa::contracts)
target_compile_definitions(loa_contracts_tests PRIVATE
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
