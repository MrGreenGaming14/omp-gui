# doctest: system package first, pinned FetchContent fallback.
include_guard(GLOBAL)

find_package(doctest CONFIG QUIET)
if(NOT TARGET doctest::doctest)
  include(FetchContent)
  FetchContent_Declare(doctest
    GIT_REPOSITORY https://github.com/doctest/doctest.git
    GIT_TAG v2.4.11
    GIT_SHALLOW TRUE)
  FetchContent_MakeAvailable(doctest)
  if(NOT TARGET doctest::doctest)
    add_library(doctest::doctest ALIAS doctest)
  endif()
endif()

if(NOT TARGET loa_doctest_main)
  set(_loa_main "${CMAKE_BINARY_DIR}/loa_doctest_main.cpp")
  file(WRITE "${_loa_main}.in"
    "#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN\n#include <doctest/doctest.h>\n")
  configure_file("${_loa_main}.in" "${_loa_main}" COPYONLY)
  add_library(loa_doctest_main STATIC "${_loa_main}")
  target_link_libraries(loa_doctest_main PUBLIC doctest::doctest)
endif()

# loa_add_test(<name> SOURCES <files...> [LINK <targets...>])
# Builds loa_<name>_tests in the current binary dir and registers it with ctest.
function(loa_add_test name)
  cmake_parse_arguments(ARG "" "" "SOURCES;LINK" ${ARGN})
  if(NOT ARG_SOURCES)
    message(FATAL_ERROR "loa_add_test(${name}): SOURCES is required")
  endif()
  set(target "loa_${name}_tests")
  add_executable(${target} ${ARG_SOURCES})
  set_target_properties(${target} PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}")
  target_link_libraries(${target} PRIVATE loa_doctest_main doctest::doctest ${ARG_LINK})
  add_test(NAME ${target} COMMAND ${target})
endfunction()
