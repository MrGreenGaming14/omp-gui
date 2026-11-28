find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)
find_package(PkgConfig REQUIRED)
pkg_check_modules(VTERM REQUIRED IMPORTED_TARGET vterm)

# The terminal tab: a libvterm emulator drawn on a Qt canvas, xterm key encoding, local PTYs (forkpty;
# ConPTY on Windows) and remote ones through the `loa::view` RemotePty port. Port of `crates/loa-ui-terminal`.
add_library(loa_ui_terminal STATIC
  src/emulator.cpp
  src/input.cpp
  src/palette.cpp
  src/runner.cpp
  src/state.cpp
  src/view.cpp
  local/local_shell.cpp
  include/loa/ui/terminal/view.hpp)
add_library(loa::ui_terminal ALIAS loa_ui_terminal)
set_target_properties(loa_ui_terminal PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_terminal PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_terminal
  PUBLIC loa::ui_kit loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets
  PRIVATE PkgConfig::VTERM)
if(UNIX)
  target_link_libraries(loa_ui_terminal PRIVATE util)
endif()

loa_add_test(ui_terminal
  SOURCES tests/input_test.cpp tests/state_test.cpp tests/local_shell_test.cpp tests/view_test.cpp
  LINK loa::ui_terminal)
set_tests_properties(loa_ui_terminal_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
