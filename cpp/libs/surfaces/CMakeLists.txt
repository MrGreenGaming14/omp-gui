find_package(PNG REQUIRED)
find_package(JPEG REQUIRED)
find_package(ZLIB REQUIRED)

add_library(loa_surfaces STATIC
  src/feed.cpp
  src/pixels.cpp
  src/proto/error.cpp
  src/proto/fragment.cpp
  src/proto/server.cpp
  src/proto/transport.cpp
  src/proto/viewer.cpp
  src/proto/wire.cpp)
add_library(loa::surfaces ALIAS loa_surfaces)
target_include_directories(loa_surfaces PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_surfaces PUBLIC loa::contracts loa::runtime
  PRIVATE PNG::PNG JPEG::JPEG ZLIB::ZLIB)

# Test doubles shared by the surface modules' tests (Rust feature `testkit`).
add_library(loa_surfaces_testkit STATIC src/testkit.cpp)
add_library(loa::surfaces_testkit ALIAS loa_surfaces_testkit)
target_link_libraries(loa_surfaces_testkit PUBLIC loa::surfaces loa::runtime)

loa_add_test(surfaces
  SOURCES
    tests/feed_test.cpp
    tests/pixels_test.cpp
    tests/testkit_test.cpp
    tests/proto_wire.cpp
    tests/proto_fragment.cpp
    tests/proto_session.cpp
  LINK loa::surfaces_testkit)
target_include_directories(loa_surfaces_tests PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_compile_definitions(loa_surfaces_tests PRIVATE
  LOA_FIXTURE_DIR="${CMAKE_SOURCE_DIR}/tests/fixtures"
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
