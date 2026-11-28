add_library(loa_projects STATIC
  src/context.cpp
  src/digest.cpp
  src/digester.cpp
  src/error.cpp
  src/links.cpp
  src/rank.cpp
  src/read.cpp
  src/rebuild.cpp
  src/search.cpp
  src/service.cpp
  src/sessions.cpp
  src/settings.cpp
  src/source.cpp
  src/text.cpp
  src/tools.cpp)
add_library(loa::projects ALIAS loa_projects)
target_include_directories(loa_projects PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_projects PUBLIC loa::bus loa::contracts loa::settings loa::core loa::runtime)

# Discovery visits projects after core, so its testkit option is known here.
if(NOT DEFINED LOA_CORE_TESTKIT OR LOA_CORE_TESTKIT)
  loa_add_test(projects
    SOURCES
      tests/projects_test.cpp
      tests/store_test.cpp
      tests/service_test.cpp
      tests/golden_test.cpp
    LINK loa::projects loa::core_testkit)
  target_compile_definitions(loa_projects_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
endif()
