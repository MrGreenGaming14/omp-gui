find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# Settings-style screens the app composes, one subdirectory per screen (`<screen>/include`,
# `<screen>/src`, `<screen>/tests`): projects and New project, agent definitions and workflows.
# Each reads the `loa::view::settings` view models and emits its screen's intent. Shared helpers
# (the crate root of `crates/loa-ui-settings/src/lib.rs`) live in `src/`. State code is plain
# C++; the views are Qt widgets.
file(GLOB _loa_ui_settings_sources CONFIGURE_DEPENDS
  "${CMAKE_CURRENT_SOURCE_DIR}/src/*.cpp"
  "${CMAKE_CURRENT_SOURCE_DIR}/*/src/*.cpp")
file(GLOB _loa_ui_settings_views CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/*/include/loa/ui/settings/*/view.hpp")
file(GLOB _loa_ui_settings_includes LIST_DIRECTORIES true CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/*/include")
list(SORT _loa_ui_settings_sources)
list(SORT _loa_ui_settings_includes)

add_library(loa_ui_settings STATIC ${_loa_ui_settings_sources} ${_loa_ui_settings_views})
add_library(loa::ui_settings ALIAS loa_ui_settings)
set_target_properties(loa_ui_settings PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_settings PUBLIC ${_loa_ui_settings_includes} PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_link_libraries(loa_ui_settings PUBLIC loa::ui_kit loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets)

file(GLOB _loa_ui_settings_tests CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/*/tests/*.cpp")
list(SORT _loa_ui_settings_tests)
loa_add_test(ui_settings SOURCES ${_loa_ui_settings_tests} LINK loa::ui_settings loa::ui_kit_testkit)
set_tests_properties(loa_ui_settings_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
