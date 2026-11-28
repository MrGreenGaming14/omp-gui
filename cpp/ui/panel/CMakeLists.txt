find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# Right panel panes: diff (unified diff parsing, Myers intra-line ranges, folds, virtualized
# rows), files (lazy tree over the `loa::view` FileSystem port), session tree (fork families),
# agents and background jobs (`<job>`s of the session DOM) and the tools page (catalog, forms,
# servers, hosts and peers). Port of `crates/loa-ui-panel`. State code is plain C++; the views
# are Qt widgets.
add_library(loa_ui_panel STATIC
  src/agents.cpp
  src/agents_view.cpp
  src/diff_model.cpp
  src/diff_parse.cpp
  src/diff_state.cpp
  src/files.cpp
  src/session_tree.cpp
  src/tools/catalog.cpp
  src/tools/form.cpp
  src/tools/state.cpp
  src/tools/view/hosts.cpp
  src/tools/view/mod.cpp
  src/tools/view/peers.cpp
  src/tools/view/servers.cpp
  src/view.cpp
  include/loa/ui/panel/agents_view.hpp
  include/loa/ui/panel/tools_view.hpp
  include/loa/ui/panel/view.hpp)
add_library(loa::ui_panel ALIAS loa_ui_panel)
set_target_properties(loa_ui_panel PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_panel PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_panel PUBLIC loa::ui_kit loa::ui_shell loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets)

loa_add_test(ui_panel
  SOURCES tests/agents_test.cpp tests/tools_test.cpp tests/tools_view_test.cpp tests/diff_test.cpp tests/files_test.cpp
          tests/session_tree_test.cpp tests/view_test.cpp
  LINK loa::ui_panel loa::ui_kit_testkit)
set_tests_properties(loa_ui_panel_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
