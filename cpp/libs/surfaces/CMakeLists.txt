find_package(PNG REQUIRED)
find_package(JPEG REQUIRED)

add_library(loa_surfaces STATIC
  src/feed.cpp
  src/pixels.cpp)
add_library(loa::surfaces ALIAS loa_surfaces)
target_include_directories(loa_surfaces PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_surfaces PUBLIC loa::contracts PRIVATE PNG::PNG JPEG::JPEG)

# Test doubles shared by the surface modules' tests (Rust feature `testkit`).
add_library(loa_surfaces_testkit STATIC src/testkit.cpp)
add_library(loa::surfaces_testkit ALIAS loa_surfaces_testkit)
target_link_libraries(loa_surfaces_testkit PUBLIC loa::surfaces loa::runtime)

loa_add_test(surfaces
  SOURCES
    tests/feed_test.cpp
    tests/pixels_test.cpp
    tests/testkit_test.cpp
  LINK loa::surfaces_testkit)
target_compile_definitions(loa_surfaces_tests PRIVATE
  LOA_FIXTURE_DIR="${CMAKE_SOURCE_DIR}/tests/fixtures")
