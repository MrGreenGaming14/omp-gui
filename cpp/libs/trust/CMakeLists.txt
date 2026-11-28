find_package(OpenSSL 3 REQUIRED COMPONENTS Crypto)

add_library(loa_trust STATIC
  src/key.cpp
  src/keystore.cpp
  src/os_keychain.cpp
  src/private_file.cpp)
add_library(loa::trust ALIAS loa_trust)
target_include_directories(loa_trust PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_trust PUBLIC loa::runtime PRIVATE loa::bus OpenSSL::Crypto)
if(APPLE)
  target_link_libraries(loa_trust PRIVATE "-framework Security" "-framework CoreFoundation")
elseif(WIN32)
  target_link_libraries(loa_trust PRIVATE advapi32)
else()
  find_package(PkgConfig REQUIRED)
  pkg_check_modules(LIBSECRET REQUIRED IMPORTED_TARGET libsecret-1)
  target_link_libraries(loa_trust PRIVATE PkgConfig::LIBSECRET)
endif()

loa_add_test(trust SOURCES tests/key.cpp tests/keystore.cpp LINK loa::trust loa::bus loa::runtime)
target_compile_definitions(loa_trust_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
