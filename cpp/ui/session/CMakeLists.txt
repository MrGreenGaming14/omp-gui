find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# The session screen. Port of `crates/loa-ui-session`; one state/view pair per screen
# (`composer/`). State code is plain C++; the views are Qt widgets.
add_library(loa_ui_session STATIC
  src/composer/editor.cpp
  src/composer/highlight.cpp
  src/composer/input.cpp
  src/composer/state.cpp
  src/composer/view.cpp
  src/composer/working.cpp
  include/loa/ui/session/composer/view.hpp)
add_library(loa::ui_session ALIAS loa_ui_session)
set_target_properties(loa_ui_session PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_session PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_session PUBLIC loa::ui_kit loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets)

loa_add_test(ui_session
  SOURCES
    tests/composer/composer_test.cpp
    tests/composer/highlight_test.cpp
    tests/composer/input_test.cpp
    tests/composer/view_test.cpp
    tests/composer/working_test.cpp
  LINK loa::ui_session loa::core_testkit)
set_tests_properties(loa_ui_session_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
