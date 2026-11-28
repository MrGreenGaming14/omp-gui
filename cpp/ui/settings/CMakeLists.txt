find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# Settings-style screens the app composes over the `loa::view` settings view models. Port of
# `crates/loa-ui-settings`; each screen keeps its state (plain C++) apart from its Qt view.
add_library(loa_ui_settings STATIC
  src/models/picker.cpp
  src/models/settings.cpp
  src/models/signin.cpp
  src/models/view.cpp
  include/loa/ui/settings/models/view.hpp)
add_library(loa::ui_settings ALIAS loa_ui_settings)
set_target_properties(loa_ui_settings PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_settings PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_settings PUBLIC loa::ui_kit loa::ui_shell loa::view Qt6::Core Qt6::Gui Qt6::Widgets)

loa_add_test(ui_settings
  SOURCES tests/models/picker_test.cpp tests/models/settings_test.cpp tests/models/signin_test.cpp tests/models/support.cpp
  LINK loa::ui_settings loa::ui_kit_testkit)
set_tests_properties(loa_ui_settings_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
