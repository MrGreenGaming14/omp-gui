add_library(loa_models STATIC src/service.cpp)
add_library(loa::models ALIAS loa_models)
target_include_directories(loa_models PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_models PUBLIC loa::settings loa::contracts loa::runtime)

# The fakes of `loa/models/testing.hpp`, for this library's and the app's tests only.
add_library(loa_models_testing STATIC src/testing.cpp)
add_library(loa::models_testing ALIAS loa_models_testing)
target_link_libraries(loa_models_testing PUBLIC loa::models)

loa_add_test(models
  SOURCES tests/service_test.cpp tests/testing_test.cpp
  LINK loa::models loa::models_testing)
target_compile_definitions(loa_models_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
