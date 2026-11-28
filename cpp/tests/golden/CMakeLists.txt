find_package(OpenSSL REQUIRED)

loa_add_test(golden SOURCES golden_test.cpp LINK OpenSSL::Crypto)
target_compile_definitions(loa_golden_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_CURRENT_SOURCE_DIR}")
