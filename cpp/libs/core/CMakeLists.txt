option(LOA_CORE_TESTKIT "Build loa::core_testkit (shared test fixtures) and loa_core_tests" ON)

add_library(loa_core STATIC
  src/base64.cpp
  src/dom.cpp
  src/patch.cpp
  src/rewind.cpp
  src/schema.cpp
  src/sink.cpp
  src/value.cpp
  src/writer.cpp
  src/xml.cpp)
add_library(loa::core ALIAS loa_core)
target_include_directories(loa_core PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_core PUBLIC loa::runtime)

if(LOA_CORE_TESTKIT)
  add_library(loa_core_testkit STATIC src/testkit.cpp)
  add_library(loa::core_testkit ALIAS loa_core_testkit)
  target_link_libraries(loa_core_testkit PUBLIC loa::core)

  loa_add_test(core SOURCES tests/core_test.cpp LINK loa::core_testkit loa::runtime)
  target_compile_definitions(loa_core_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
endif()
