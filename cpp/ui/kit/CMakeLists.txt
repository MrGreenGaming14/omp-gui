find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets Svg)
find_package(tomlplusplus REQUIRED)

option(LOA_UI_KIT_TESTKIT "Build loa::ui_kit_testkit and loa_ui_kit_tests" ON)

add_library(loa_ui_kit STATIC
  src/icons.cpp
  src/popover.cpp
  src/qt.cpp
  src/style.cpp
  src/text.cpp
  src/theme/builtin.cpp
  src/theme/theme.cpp
  src/theme/tokens.cpp
  src/virtual_list.cpp
  src/widgets.cpp)
add_library(loa::ui_kit ALIAS loa_ui_kit)
target_include_directories(loa_ui_kit PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_kit
  PUBLIC loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets
  PRIVATE Qt6::Svg tomlplusplus::tomlplusplus)

# The 37 Lucide SVGs, compiled into every binary that links the kit as `:/loa/icons/<name>.svg`.
file(GLOB LOA_UI_KIT_ICONS CONFIGURE_DEPENDS RELATIVE "${CMAKE_CURRENT_SOURCE_DIR}" "${CMAKE_CURRENT_SOURCE_DIR}/icons/*.svg")
list(SORT LOA_UI_KIT_ICONS)
qt_add_resources(loa_ui_kit "loa_ui_kit_icons" PREFIX "/loa" FILES ${LOA_UI_KIT_ICONS})

if(LOA_UI_KIT_TESTKIT)
  add_library(loa_ui_kit_testkit STATIC src/testkit.cpp)
  add_library(loa::ui_kit_testkit ALIAS loa_ui_kit_testkit)
  target_link_libraries(loa_ui_kit_testkit PUBLIC loa::ui_kit)

  loa_add_test(ui_kit
    SOURCES tests/theme_test.cpp tests/text_test.cpp tests/widgets_test.cpp tests/icons_test.cpp
    LINK loa::ui_kit_testkit tomlplusplus::tomlplusplus Qt6::Svg)
  set_tests_properties(loa_ui_kit_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
endif()

# Theme lint: no colour or size literals outside the token layer in cpp/ui and cpp/app.
add_test(NAME loa_ui_lint_literals
  COMMAND "${CMAKE_SOURCE_DIR}/scripts/lint-ui-literals.sh"
  WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}")
set_tests_properties(loa_ui_lint_literals PROPERTIES ENVIRONMENT "LINT_ENFORCE=1")
