add_library(loa_app_runtime STATIC
  src/feed.cpp
  src/harnesses.cpp
  src/lifecycle.cpp
  src/runtime.cpp
  src/session.cpp)
add_library(loa::app_runtime ALIAS loa_app_runtime)
target_include_directories(loa_app_runtime PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_app_runtime PUBLIC loa::bus loa::contracts loa::core loa::runtime loa::session)

loa_add_test(app_runtime SOURCES tests/lifecycle.cpp tests/feed.cpp
  LINK loa::app_runtime loa::harness_testing)
target_compile_definitions(loa_app_runtime_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
