find_package(CURL REQUIRED)
find_package(LibArchive REQUIRED)
find_package(OpenSSL 3 REQUIRED COMPONENTS Crypto)
find_library(LOA_CPP_HTTPLIB_LIBRARY NAMES cpp-httplib REQUIRED)

add_library(loa_updater STATIC
  src/decision.cpp
  src/error.cpp
  src/feeds.cpp
  src/gates.cpp
  src/http.cpp
  src/model.cpp
  src/peers.cpp
  src/service.cpp
  src/signature.cpp
  src/stage.cpp
  src/state.cpp
  src/swap.cpp
  src/unpack.cpp
  src/util.cpp)
add_library(loa::updater ALIAS loa_updater)
target_include_directories(loa_updater PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_updater PUBLIC loa::settings loa::runtime PRIVATE CURL::libcurl LibArchive::LibArchive OpenSSL::Crypto)

loa_add_test(updater SOURCES tests/gates.cpp tests/standing.cpp tests/service.cpp tests/golden.cpp LINK loa::updater "${LOA_CPP_HTTPLIB_LIBRARY}")
target_compile_definitions(loa_updater_tests PRIVATE
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
