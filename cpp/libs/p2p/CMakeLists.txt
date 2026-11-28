# loa::p2p — wire codec, transports (iroh QUIC through loa::iroh_ffi), discovery.
include(FetchContent)

# mjansson/mdns: one header of static functions, included by src/discovery/mdns.cpp only.
FetchContent_Declare(loa_mdns
  URL https://github.com/mjansson/mdns/archive/refs/tags/1.4.3.tar.gz
  URL_HASH SHA256=be1fd8e35599cb7de179decbd0633c121d11a2dcb9cc193ff5c590bd0d480483)
FetchContent_GetProperties(loa_mdns)
if(NOT loa_mdns_POPULATED)
  FetchContent_Populate(loa_mdns)
endif()

find_package(OpenSSL 3 REQUIRED COMPONENTS Crypto SSL)
find_package(SQLite3 REQUIRED)

add_library(loa_p2p STATIC
  src/auth.cpp
  src/discovery/discovery.cpp
  src/discovery/mdns.cpp
  src/discovery/tailnet.cpp
  src/node.cpp
  src/peers_db.cpp
  src/protocols/control.cpp
  src/protocols/extra.cpp
  src/protocols/folder.cpp
  src/protocols/host.cpp
  src/protocols/projection.cpp
  src/protocols/session.cpp
  src/quic.cpp
  src/sessions.cpp
  src/tcp.cpp
  src/transport.cpp
  src/wire.cpp
  src/yamux.cpp)
add_library(loa::p2p ALIAS loa_p2p)
target_include_directories(loa_p2p PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
set_source_files_properties(src/discovery/mdns.cpp PROPERTIES INCLUDE_DIRECTORIES "${loa_mdns_SOURCE_DIR}")
target_link_libraries(loa_p2p PUBLIC loa::runtime loa::trust loa::core loa::bus PRIVATE loa::iroh_ffi OpenSSL::SSL OpenSSL::Crypto SQLite::SQLite3)

loa_add_test(p2p SOURCES tests/discovery.cpp tests/node.cpp tests/protocols.cpp tests/projection.cpp tests/tcp.cpp tests/transport.cpp
  tests/wire.cpp tests/yamux.cpp
  LINK loa::p2p loa::bus loa::core loa::iroh_ffi OpenSSL::SSL OpenSSL::Crypto SQLite::SQLite3)
target_compile_definitions(loa_p2p_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
