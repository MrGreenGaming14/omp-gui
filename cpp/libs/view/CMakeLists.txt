find_package(tomlplusplus REQUIRED)

add_library(loa_view STATIC src/view.cpp)
add_library(loa::view ALIAS loa_view)
target_include_directories(loa_view PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_view PUBLIC loa::contracts loa::core tomlplusplus::tomlplusplus)

# The public-API gate: lists include/loa/view and compares with api.txt.
add_library(loa_view_api_listing STATIC tools/api_listing.cpp)
target_include_directories(loa_view_api_listing PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/tools")
add_executable(loa_view_api tools/api_main.cpp)
target_link_libraries(loa_view_api PRIVATE loa_view_api_listing loa::view)
add_test(NAME loa_view_api
  COMMAND loa_view_api "${CMAKE_CURRENT_SOURCE_DIR}/include/loa/view" "${CMAKE_CURRENT_SOURCE_DIR}/api.txt")

loa_add_test(view SOURCES tests/view_test.cpp LINK loa::view loa_view_api_listing)
target_compile_definitions(loa_view_tests PRIVATE
  LOA_VIEW_INCLUDE_DIR="${CMAKE_CURRENT_SOURCE_DIR}/include/loa/view"
  LOA_VIEW_API_TXT="${CMAKE_CURRENT_SOURCE_DIR}/api.txt")
