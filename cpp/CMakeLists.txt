cmake_minimum_required(VERSION 3.25)

if(NOT DEFINED CMAKE_CXX_COMPILER_LAUNCHER)
  find_program(LOA_CCACHE ccache)
  if(LOA_CCACHE)
    set(CMAKE_CXX_COMPILER_LAUNCHER "${LOA_CCACHE}" CACHE STRING "")
    set(CMAKE_C_COMPILER_LAUNCHER "${LOA_CCACHE}" CACHE STRING "")
  endif()
endif()

project(omp_gui LANGUAGES C CXX)

if(NOT CMAKE_GENERATOR MATCHES "Ninja")
  message(WARNING "omp-gui is built with Ninja (-G Ninja); generator '${CMAKE_GENERATOR}' is unsupported")
endif()

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

# Fail early and clearly when the compiler cannot do C++23.
if(NOT "cxx_std_23" IN_LIST CMAKE_CXX_COMPILE_FEATURES)
  message(FATAL_ERROR
    "omp-gui requires a C++23 compiler; ${CMAKE_CXX_COMPILER_ID} "
    "${CMAKE_CXX_COMPILER_VERSION} does not support cxx_std_23 (use GCC >= 13 or Clang >= 17).")
endif()
include(CheckCXXSourceCompiles)
set(CMAKE_REQUIRED_FLAGS "${CMAKE_CXX23_STANDARD_COMPILE_OPTION}")
check_cxx_source_compiles([=[
#include <expected>
#include <format>
#include <span>
int main() {
  std::expected<int, int> e{1};
  int a[1]{};
  std::span<int> s{a};
  return static_cast<int>(std::format("{}", *e).size() + s.size()) - 2;
}
]=] LOA_HAS_CXX23_LIBRARY)
unset(CMAKE_REQUIRED_FLAGS)
if(NOT LOA_HAS_CXX23_LIBRARY)
  message(FATAL_ERROR
    "omp-gui requires C++23 standard library support (std::expected, std::format, std::span); "
    "${CMAKE_CXX_COMPILER_ID} ${CMAKE_CXX_COMPILER_VERSION} with its standard library lacks it.")
endif()

list(APPEND CMAKE_MODULE_PATH "${CMAKE_CURRENT_SOURCE_DIR}/cmake")
enable_testing()
include(LoaTest)

# Auto-discovery of libs/*, ui/*, tests/* in sorted order.
set(LOA_DISCOVERED "")
foreach(group libs ui tests)
  file(GLOB _candidates LIST_DIRECTORIES true CONFIGURE_DEPENDS
       "${CMAKE_CURRENT_SOURCE_DIR}/${group}/*/CMakeLists.txt")
  list(SORT _candidates)
  foreach(_cml IN LISTS _candidates)
    get_filename_component(_dir "${_cml}" DIRECTORY)
    file(RELATIVE_PATH _rel "${CMAKE_CURRENT_SOURCE_DIR}" "${_dir}")
    list(APPEND LOA_DISCOVERED "${_rel}")
  endforeach()
endforeach()
# The `loa-qt` application (cpp/app) links every library, so it comes last.
if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/app/CMakeLists.txt")
  list(APPEND LOA_DISCOVERED app)
endif()
foreach(_rel IN LISTS LOA_DISCOVERED)
  add_subdirectory("${_rel}" "${CMAKE_CURRENT_BINARY_DIR}/${_rel}")
endforeach()
list(JOIN LOA_DISCOVERED "\n" _discovered_text)
if(_discovered_text)
  string(APPEND _discovered_text "\n")
endif()
file(WRITE "${CMAKE_BINARY_DIR}/loa_discovered.txt" "${_discovered_text}")
list(LENGTH LOA_DISCOVERED _n)
message(STATUS "omp-gui: discovered ${_n} subdirectories (see loa_discovered.txt)")
