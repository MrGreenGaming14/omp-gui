find_package(OpenSSL REQUIRED)
find_package(SQLite3 REQUIRED)

loa_add_test(golden SOURCES golden_test.cpp LINK OpenSSL::Crypto SQLite::SQLite3)
target_compile_definitions(loa_golden_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_CURRENT_SOURCE_DIR}")
