find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)
# md4c via pkg-config: Ubuntu's md4cConfig.cmake references a missing libmd4c-html.
find_package(PkgConfig REQUIRED)
pkg_check_modules(MD4C REQUIRED IMPORTED_TARGET md4c)

# The session screen. Port of `crates/loa-ui-session`; one state/view pair per screen
# (`composer/`, `transcript/`). State code is plain C++; the views are Qt widgets.
add_library(loa_ui_session STATIC
  src/composer/editor.cpp
  src/composer/highlight.cpp
  src/composer/input.cpp
  src/composer/state.cpp
  src/composer/view.cpp
  src/composer/working.cpp
  src/transcript/items.cpp
  src/transcript/prose.cpp
  src/transcript/rows.cpp
  src/transcript/selection.cpp
  src/transcript/state.cpp
  src/transcript/view.cpp
  include/loa/ui/session/composer/view.hpp
  include/loa/ui/session/transcript/view.hpp)
add_library(loa::ui_session ALIAS loa_ui_session)
set_target_properties(loa_ui_session PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_session PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_session PUBLIC loa::ui_kit loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets
  PRIVATE PkgConfig::MD4C)

loa_add_test(ui_session
  SOURCES
    tests/composer/composer_test.cpp
    tests/composer/highlight_test.cpp
    tests/composer/input_test.cpp
    tests/composer/view_test.cpp
    tests/composer/working_test.cpp
    tests/transcript/prose_test.cpp
    tests/transcript/selection_test.cpp
    tests/transcript/transcript_test.cpp
    tests/transcript/view_test.cpp
  LINK loa::ui_session loa::core_testkit loa::ui_kit_testkit)
set_tests_properties(loa_ui_session_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
