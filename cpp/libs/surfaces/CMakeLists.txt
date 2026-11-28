find_package(PNG REQUIRED)
find_package(JPEG REQUIRED)
find_package(GIF REQUIRED)
find_package(PkgConfig REQUIRED)
pkg_check_modules(WEBP REQUIRED IMPORTED_TARGET libwebp)
find_package(Qt6 REQUIRED COMPONENTS Core Gui Pdf)
find_package(ZLIB REQUIRED)
find_package(OpenSSL 3 REQUIRED COMPONENTS Crypto)
find_package(pugixml REQUIRED)
pkg_check_modules(MINIZIP REQUIRED IMPORTED_TARGET minizip)
pkg_check_modules(XLSXWRITER REQUIRED IMPORTED_TARGET xlsxwriter)

add_library(loa_surfaces STATIC
  src/feed.cpp
  src/pixels.cpp
  src/docs/docx.cpp
  src/docs/format.cpp
  src/docs/images.cpp
  src/docs/ooxml.cpp
  src/docs/pdf.cpp
  src/docs/pptx.cpp
  src/docs/raster.cpp
  src/docs/server.cpp
  src/docs/xlsx.cpp
  src/proto/error.cpp
  src/proto/fragment.cpp
  src/proto/server.cpp
  src/proto/transport.cpp
  src/proto/viewer.cpp
  src/proto/wire.cpp
  src/vnc/decode.cpp
  src/vnc/input.cpp
  src/vnc/keysym.cpp
  src/vnc/proto.cpp
  src/vnc/surface.cpp)
add_library(loa::surfaces ALIAS loa_surfaces)
target_include_directories(loa_surfaces PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_surfaces PUBLIC loa::contracts loa::runtime pugixml::pugixml
  PRIVATE PNG::PNG JPEG::JPEG GIF::GIF PkgConfig::WEBP Qt6::Pdf Qt6::Gui ZLIB::ZLIB OpenSSL::Crypto PkgConfig::MINIZIP)
# Only src/docs/pdf.cpp includes Qt; keep moc and friends off the rest.
set_target_properties(loa_surfaces PROPERTIES AUTOMOC OFF AUTOUIC OFF AUTORCC OFF)

# Test doubles shared by the surface modules' tests (Rust feature `testkit`).
add_library(loa_surfaces_testkit STATIC
  src/testkit.cpp
  src/vnc/testkit.cpp)
add_library(loa::surfaces_testkit ALIAS loa_surfaces_testkit)
target_link_libraries(loa_surfaces_testkit PUBLIC loa::surfaces loa::runtime PRIVATE ZLIB::ZLIB)

loa_add_test(surfaces
  SOURCES
    tests/docs_test.cpp
    tests/docx_test.cpp
    tests/ooxml_test.cpp
    tests/pptx_test.cpp
    tests/xlsx_test.cpp
    tests/feed_test.cpp
    tests/pixels_test.cpp
    tests/testkit_test.cpp
    tests/proto_wire.cpp
    tests/proto_fragment.cpp
    tests/proto_session.cpp
    tests/vnc_codec_test.cpp
    tests/vnc_surface_test.cpp
  LINK loa::surfaces_testkit GIF::GIF JPEG::JPEG PkgConfig::WEBP PkgConfig::MINIZIP PkgConfig::XLSXWRITER)
target_include_directories(loa_surfaces_tests PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_compile_definitions(loa_surfaces_tests PRIVATE
  LOA_FIXTURE_DIR="${CMAKE_SOURCE_DIR}/tests/fixtures"
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
