add_library(loa_harness STATIC
  src/kit/jsonrpc.cpp
  src/kit/ledger.cpp
  src/kit/mapper.cpp
  src/kit/native.cpp
  src/kit/proc.cpp
  src/kit/residual.cpp
  src/kit/rpc.cpp
  src/native.cpp
  src/account_support/account_support.cpp
  src/account_support/cli.cpp
  src/account_support/configdoc.cpp
  src/account_support/opener.cpp
  src/account_support/process.cpp
  src/codex/account.cpp
  src/codex/adapter.cpp
  src/codex/mapper.cpp
  src/codex/rollout.cpp
  src/codex/session.cpp
  src/registry/registry.cpp)
add_library(loa::harness ALIAS loa_harness)
target_include_directories(loa_harness PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_harness PUBLIC loa::contracts loa::core loa::runtime loa::settings)

# Test doubles (the Rust `testing` feature): linked by adapter and app tests only.
add_library(loa_harness_testing STATIC
  src/testing/dom.cpp
  src/testing/scripted.cpp
  src/testing/testing.cpp)
add_library(loa::harness_testing ALIAS loa_harness_testing)
target_link_libraries(loa_harness_testing PUBLIC loa::harness)

loa_add_test(harness
  SOURCES
    tests/kit/jsonrpc_test.cpp
    tests/kit/ledger_test.cpp
    tests/kit/mapper_test.cpp
    tests/kit/proc_test.cpp
    tests/kit/residual_test.cpp
    tests/kit/rpc_test.cpp
    tests/native/native_test.cpp
    tests/account_support/configdoc_test.cpp
    tests/account_support/opener_test.cpp
    tests/account_support/process_test.cpp
    tests/registry/registry_test.cpp
    tests/testing/dom_test.cpp
    tests/codex/account.cpp
    tests/codex/adapter.cpp
    tests/codex/mapper.cpp
    tests/codex/rollout.cpp
    tests/codex/session.cpp
    tests/testing/scripted_test.cpp
  LINK loa::harness loa::harness_testing)
target_compile_definitions(loa_harness_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden"
  LOA_HARNESS_SOURCE_DIR="${CMAKE_CURRENT_SOURCE_DIR}")
