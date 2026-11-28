find_package(ZLIB REQUIRED)

add_library(loa_surfaces STATIC
  src/proto/error.cpp
  src/proto/fragment.cpp
  src/proto/server.cpp
  src/proto/transport.cpp
  src/proto/viewer.cpp
  src/proto/wire.cpp)
add_library(loa::surfaces ALIAS loa_surfaces)
target_include_directories(loa_surfaces PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_surfaces PUBLIC loa::contracts loa::runtime PRIVATE ZLIB::ZLIB)

loa_add_test(surfaces
  SOURCES
    tests/proto_wire.cpp
    tests/proto_fragment.cpp
    tests/proto_session.cpp
  LINK loa::surfaces)
target_include_directories(loa_surfaces_tests PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_compile_definitions(loa_surfaces_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
