add_library(loa_harness STATIC
  src/kit/jsonrpc.cpp
  src/kit/ledger.cpp
  src/kit/mapper.cpp
  src/kit/native.cpp
  src/kit/proc.cpp
  src/kit/residual.cpp
  src/kit/rpc.cpp
  src/native.cpp)
add_library(loa::harness ALIAS loa_harness)
target_include_directories(loa_harness PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_harness PUBLIC loa::contracts loa::core loa::runtime)

loa_add_test(harness
  SOURCES
    tests/kit/jsonrpc_test.cpp
    tests/kit/ledger_test.cpp
    tests/kit/mapper_test.cpp
    tests/kit/proc_test.cpp
    tests/kit/residual_test.cpp
    tests/kit/rpc_test.cpp
    tests/native/native_test.cpp
  LINK loa::harness)
target_compile_definitions(loa_harness_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
