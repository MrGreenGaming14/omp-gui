find_package(OpenSSL 3 REQUIRED COMPONENTS Crypto)
find_package(ZLIB REQUIRED)

add_library(loa_bus STATIC
  src/bus.cpp
  src/frame.cpp
  src/journal.cpp
  src/journal_io.cpp
  src/proto.cpp
  src/snapshot.cpp
  src/store.cpp
  src/writer.cpp)
add_library(loa::bus ALIAS loa_bus)
target_include_directories(loa_bus PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_bus PUBLIC loa::core PRIVATE OpenSSL::Crypto ZLIB::ZLIB)
# Discovery visits bus before core, so the core option may not be cached yet.
if(NOT DEFINED LOA_CORE_TESTKIT OR LOA_CORE_TESTKIT)
  loa_add_test(bus SOURCES tests/bus.cpp tests/frame.cpp tests/journal.cpp tests/proto.cpp
    LINK loa::bus loa::core_testkit)
  target_compile_definitions(loa_bus_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
endif()
