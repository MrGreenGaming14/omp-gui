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

add_library(loa_p2p STATIC
  src/discovery/discovery.cpp
  src/discovery/mdns.cpp
  src/discovery/tailnet.cpp
  src/quic.cpp
  src/transport.cpp
  src/wire.cpp)
add_library(loa::p2p ALIAS loa_p2p)
target_include_directories(loa_p2p PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
set_source_files_properties(src/discovery/mdns.cpp PROPERTIES INCLUDE_DIRECTORIES "${loa_mdns_SOURCE_DIR}")
target_link_libraries(loa_p2p PUBLIC loa::runtime loa::trust PRIVATE loa::core loa::bus loa::iroh_ffi)

loa_add_test(p2p SOURCES tests/discovery.cpp tests/transport.cpp tests/wire.cpp
  LINK loa::p2p loa::bus loa::core loa::iroh_ffi)
target_compile_definitions(loa_p2p_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
