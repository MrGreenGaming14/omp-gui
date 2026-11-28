find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# The app frame (layout, sidebar, top bar, theme), the command palette and the keymap.
# Port of `crates/loa-ui-shell`. State code is plain C++; the views are Qt widgets.
add_library(loa_ui_shell STATIC
  src/keymap.cpp
  src/layout.cpp
  src/palette.cpp
  src/sidebar.cpp
  src/theme.cpp
  src/view.cpp
  include/loa/ui/shell/view.hpp)
add_library(loa::ui_shell ALIAS loa_ui_shell)
set_target_properties(loa_ui_shell PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_shell PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_shell PUBLIC loa::ui_kit loa::view Qt6::Core Qt6::Gui Qt6::Widgets)

loa_add_test(ui_shell
  SOURCES tests/keymap_test.cpp tests/palette_test.cpp tests/sidebar_test.cpp tests/layout_test.cpp tests/view_test.cpp
  LINK loa::ui_shell)
set_tests_properties(loa_ui_shell_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
