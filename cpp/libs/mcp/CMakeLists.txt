find_package(CURL REQUIRED)
find_package(PkgConfig REQUIRED)
pkg_check_modules(LOA_MCP_HTTPLIB REQUIRED IMPORTED_TARGET cpp-httplib)

add_library(loa_mcp STATIC
  src/bridge.cpp
  src/central.cpp
  src/connect.cpp
  src/error.cpp
  src/host_tools.cpp
  src/http.cpp
  src/httpd.cpp
  src/import.cpp
  src/manager.cpp
  src/model.cpp
  src/provider.cpp
  src/registry.cpp
  src/spec.cpp
  src/stdio.cpp
  src/toolkit.cpp)
add_library(loa::mcp ALIAS loa_mcp)
target_include_directories(loa_mcp PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_mcp
  PUBLIC loa::contracts loa::settings loa::runtime
  PRIVATE loa::core loa::harness CURL::libcurl PkgConfig::LOA_MCP_HTTPLIB)

# The fake MCP server the client tests spawn over stdio.
add_executable(loa_mcp_fake_server tests/fake_server.cpp tests/fake.cpp)
set_target_properties(loa_mcp_fake_server PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(loa_mcp_fake_server PRIVATE loa::runtime loa::core)

loa_add_test(mcp
  SOURCES
    tests/bridge_test.cpp
    tests/central_test.cpp
    tests/client_test.cpp
    tests/fake.cpp
    tests/import_test.cpp
    tests/spec_test.cpp
    tests/toolkit_test.cpp
  LINK loa::mcp loa::core loa::harness loa::harness_testing PkgConfig::LOA_MCP_HTTPLIB)
add_dependencies(loa_mcp_tests loa_mcp_fake_server)
target_compile_definitions(loa_mcp_tests PRIVATE
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden"
  LOA_MCP_FAKE_SERVER="$<TARGET_FILE:loa_mcp_fake_server>")
