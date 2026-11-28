find_package(PNG REQUIRED)
find_package(JPEG REQUIRED)
find_package(GIF REQUIRED)
find_package(PkgConfig REQUIRED)
pkg_check_modules(WEBP REQUIRED IMPORTED_TARGET libwebp)
find_package(Qt6 REQUIRED COMPONENTS Core Gui Pdf)
find_package(ZLIB REQUIRED)
pkg_check_modules(OPENH264 REQUIRED IMPORTED_TARGET openh264)
find_package(OpenSSL 3 REQUIRED COMPONENTS Crypto)
find_package(pugixml REQUIRED)
pkg_check_modules(MINIZIP REQUIRED IMPORTED_TARGET minizip)
pkg_check_modules(XLSXWRITER REQUIRED IMPORTED_TARGET xlsxwriter)
pkg_check_modules(LOA_SURFACES_HTTPLIB REQUIRED IMPORTED_TARGET cpp-httplib)
# The Rust `remote-video` feature: the hub codes live surfaces as video for network viewers.
option(LOA_SURFACES_REMOTE_VIDEO "Build the surface hub's remote video (Rust feature remote-video)" OFF)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  pkg_check_modules(XCB REQUIRED IMPORTED_TARGET xcb)
endif()

add_library(loa_surfaces STATIC
  src/browser/cdp.cpp
  src/browser/chrome.cpp
  src/browser/error.cpp
  src/browser/input.cpp
  src/browser/keys.cpp
  src/browser/process.cpp
  src/browser/server.cpp
  src/browser/structured.cpp
  src/browser/x11.cpp
  src/browser/xvfb.cpp
  src/feed.cpp
  src/pixels.cpp
  src/hub/dom.cpp
  src/hub/endpoint.cpp
  src/hub/error.cpp
  src/hub/mcp.cpp
  src/hub/mcp_surface.cpp
  src/hub/providers.cpp
  src/hub/service.cpp
  src/hub/tools.cpp
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
  src/video/decode.cpp
  src/video/encode.cpp
  src/video/ffmpeg.cpp
  src/video/live.cpp
  src/vnc/decode.cpp
  src/vnc/input.cpp
  src/vnc/keysym.cpp
  src/vnc/proto.cpp
  src/vnc/surface.cpp)
add_library(loa::surfaces ALIAS loa_surfaces)
target_include_directories(loa_surfaces PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include"
  PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_link_libraries(loa_surfaces PUBLIC loa::contracts loa::core loa::mcp loa::runtime pugixml::pugixml
  PRIVATE PNG::PNG JPEG::JPEG GIF::GIF PkgConfig::WEBP Qt6::Pdf Qt6::Gui ZLIB::ZLIB OpenSSL::Crypto PkgConfig::OPENH264 PkgConfig::MINIZIP
    PkgConfig::LOA_SURFACES_HTTPLIB)
if(LOA_SURFACES_REMOTE_VIDEO)
  target_sources(loa_surfaces PRIVATE src/hub/video.cpp)
  target_compile_definitions(loa_surfaces PUBLIC LOA_SURFACES_REMOTE_VIDEO)
endif()
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  target_link_libraries(loa_surfaces PRIVATE PkgConfig::XCB)
endif()
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
    tests/video_test.cpp
    tests/vnc_codec_test.cpp
    tests/vnc_surface_test.cpp
    tests/browser_input_test.cpp
    tests/browser_chrome_test.cpp
    tests/browser_test.cpp
    tests/browser_x11_test.cpp
    tests/hub_test.cpp
  LINK loa::surfaces_testkit loa::bus GIF::GIF JPEG::JPEG PkgConfig::WEBP PkgConfig::MINIZIP PkgConfig::XLSXWRITER)
target_include_directories(loa_surfaces_tests PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_compile_definitions(loa_surfaces_tests PRIVATE
  LOA_FIXTURE_DIR="${CMAKE_SOURCE_DIR}/tests/fixtures"
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
