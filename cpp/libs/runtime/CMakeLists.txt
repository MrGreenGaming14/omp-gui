find_package(Threads REQUIRED)

# Standalone Asio (libasio-dev ships headers only, no CMake config).
find_path(LOA_ASIO_INCLUDE_DIR asio.hpp REQUIRED)

include(FetchContent)
FetchContent_Declare(yyjson
  URL https://github.com/ibireme/yyjson/archive/refs/tags/0.10.0.tar.gz
  URL_HASH SHA256=0d901cb2c45c5586e3f3a4245e58c2252d6b24bf4b402723f6179523d389b165)
set(YYJSON_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(YYJSON_INSTALL OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(yyjson)

add_library(loa_runtime STATIC
  src/error.cpp
  src/executor.cpp
  src/json.cpp
  src/log.cpp
  src/proto.cpp
  src/semver.cpp)
add_library(loa::runtime ALIAS loa_runtime)
target_include_directories(loa_runtime PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
# Asio is public because coroutine callers include <asio.hpp> to build `Task`s;
# loa headers themselves only forward-declare it.
target_include_directories(loa_runtime SYSTEM PUBLIC "${LOA_ASIO_INCLUDE_DIR}")
target_compile_definitions(loa_runtime PUBLIC ASIO_STANDALONE ASIO_NO_DEPRECATED)
target_link_libraries(loa_runtime PUBLIC Threads::Threads PRIVATE yyjson)

loa_add_test(runtime
  SOURCES
    tests/executor_test.cpp
    tests/json_test.cpp
    tests/log_test.cpp
    tests/proto_test.cpp
    tests/semver_test.cpp
    tests/headers_test.cpp
  LINK loa::runtime)
target_compile_definitions(loa_runtime_tests PRIVATE
  LOA_RUNTIME_INCLUDE_DIR="${CMAKE_CURRENT_SOURCE_DIR}/include/loa/runtime")
