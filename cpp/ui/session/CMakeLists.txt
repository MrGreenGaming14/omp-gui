find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# The session screen. Port of `crates/loa-ui-session`; one state/view pair per screen
# (`decisions/`). State code is plain C++; the views are Qt widgets.
add_library(loa_ui_session STATIC
  src/decisions/card.cpp
  src/decisions/desk.cpp
  src/decisions/form.cpp
  src/decisions/model.cpp
  src/decisions/state.cpp
  src/decisions/unattended.cpp
  src/decisions/view.cpp
  include/loa/ui/session/decisions/view.hpp)
add_library(loa::ui_session ALIAS loa_ui_session)
set_target_properties(loa_ui_session PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_session PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_session PUBLIC loa::ui_kit loa::ui_shell loa::ui_panel loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets)

loa_add_test(ui_session
  SOURCES
    tests/decisions/decisions_test.cpp
    tests/decisions/view_test.cpp
  LINK loa::ui_session loa::core_testkit)
set_tests_properties(loa_ui_session_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
