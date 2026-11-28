add_library(loa_harness STATIC
  src/kit/jsonrpc.cpp
  src/kit/ledger.cpp
  src/kit/mapper.cpp
  src/kit/native.cpp
  src/kit/proc.cpp
  src/kit/residual.cpp
  src/kit/rpc.cpp
  src/native.cpp
  src/omp2/mapper.cpp
  src/omp2/omp2.cpp
  src/omp2/wire.cpp
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
  src/claude/account.cpp
  src/claude/driver.cpp
  src/claude/mapper.cpp
  src/claude/mcp.cpp
  src/claude/native.cpp
  src/registry/registry.cpp)
target_sources(loa_harness PRIVATE
  src/omp/rpc/client.cpp
  src/omp/rpc/command.cpp
  src/omp/rpc/event.cpp
  src/omp/rpc/frames.cpp
  src/omp/rpc/inbound.cpp
  src/omp/native/native.cpp
  src/omp/native/import.cpp
  src/omp/native/export.cpp
  src/omp/native/history.cpp)
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
    tests/claude/account_test.cpp
    tests/claude/driver_test.cpp
    tests/claude/mapper_test.cpp
    tests/claude/mcp_test.cpp
    tests/claude/native_test.cpp
    tests/native/native_test.cpp
    tests/omp2/omp2_test.cpp
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
target_sources(loa_harness_tests PRIVATE
  tests/omp/rpc_command_test.cpp
  tests/omp/rpc_client_test.cpp
  tests/omp/rpc_event_test.cpp
  tests/omp/rpc_frames_test.cpp
  tests/omp/native_import_test.cpp
  tests/omp/native_export_test.cpp
  tests/omp/native_history_test.cpp)
target_compile_definitions(loa_harness_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden"
  LOA_HARNESS_SOURCE_DIR="${CMAKE_CURRENT_SOURCE_DIR}")
target_compile_definitions(loa_harness_tests PRIVATE LOA_HARNESS_FIXTURES="${CMAKE_SOURCE_DIR}/../crates/loa-harness/fixtures")
