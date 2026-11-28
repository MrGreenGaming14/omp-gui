find_package(PNG REQUIRED)
find_package(JPEG REQUIRED)
find_package(GIF REQUIRED)
find_package(PkgConfig REQUIRED)
pkg_check_modules(WEBP REQUIRED IMPORTED_TARGET libwebp)
find_package(Qt6 REQUIRED COMPONENTS Core Gui Pdf)

add_library(loa_surfaces STATIC
  src/feed.cpp
  src/pixels.cpp
  src/docs/format.cpp
  src/docs/images.cpp
  src/docs/pdf.cpp
  src/docs/raster.cpp
  src/docs/server.cpp)
add_library(loa::surfaces ALIAS loa_surfaces)
target_include_directories(loa_surfaces PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_surfaces PUBLIC loa::contracts
  PRIVATE PNG::PNG JPEG::JPEG GIF::GIF PkgConfig::WEBP Qt6::Pdf Qt6::Gui)
# Only src/docs/pdf.cpp includes Qt; keep moc and friends off the rest.
set_target_properties(loa_surfaces PROPERTIES AUTOMOC OFF AUTOUIC OFF AUTORCC OFF)

# Test doubles shared by the surface modules' tests (Rust feature `testkit`).
add_library(loa_surfaces_testkit STATIC src/testkit.cpp)
add_library(loa::surfaces_testkit ALIAS loa_surfaces_testkit)
target_link_libraries(loa_surfaces_testkit PUBLIC loa::surfaces loa::runtime)

loa_add_test(surfaces
  SOURCES
    tests/docs_test.cpp
    tests/feed_test.cpp
    tests/pixels_test.cpp
    tests/testkit_test.cpp
  LINK loa::surfaces_testkit GIF::GIF JPEG::JPEG PkgConfig::WEBP)
target_compile_definitions(loa_surfaces_tests PRIVATE
  LOA_FIXTURE_DIR="${CMAKE_SOURCE_DIR}/tests/fixtures"
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
