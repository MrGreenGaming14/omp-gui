find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# The app minus `main`, so tests drive the same code as `loa-qt`.
add_library(loa_app STATIC
  src/agents_smoke.cpp
  src/application.cpp
  src/asio_bridge.cpp
  src/kit_gallery.cpp
  src/local_fs.cpp
  src/panel_smoke.cpp
  src/peers_updates_smoke.cpp
  src/settings_smoke.cpp
  src/smoke.cpp
  src/surface_smoke.cpp
  src/terminal_smoke.cpp
  src/transcript_smoke.cpp)
add_library(loa::app ALIAS loa_app)
target_include_directories(loa_app PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_app PUBLIC loa::ui_kit loa::ui_session loa::ui_shell loa::ui_panel loa::ui_settings loa::ui_terminal loa::ui_surface loa::view loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets)

add_executable(loa-qt src/main.cpp)
set_target_properties(loa-qt PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(loa-qt PRIVATE loa::app)

loa_add_test(app SOURCES tests/app_test.cpp LINK loa::app)
set_tests_properties(loa_app_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")

add_test(NAME loa_qt_smoke_kit COMMAND loa-qt --smoke kit)
set_tests_properties(loa_qt_smoke_kit PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)

add_test(NAME loa_qt_smoke_shell COMMAND loa-qt --smoke shell)
set_tests_properties(loa_qt_smoke_shell PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)

add_test(NAME loa_qt_smoke_composer COMMAND loa-qt --smoke composer)
set_tests_properties(loa_qt_smoke_composer PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_panel COMMAND loa-qt --smoke panel)
set_tests_properties(loa_qt_smoke_panel PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_terminal COMMAND loa-qt --smoke terminal)
set_tests_properties(loa_qt_smoke_terminal PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_agents COMMAND loa-qt --smoke agents)
set_tests_properties(loa_qt_smoke_agents PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_transcript COMMAND loa-qt --smoke transcript)
set_tests_properties(loa_qt_smoke_transcript PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_surface COMMAND loa-qt --smoke surface)
set_tests_properties(loa_qt_smoke_surface PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_projects COMMAND loa-qt --smoke projects)
set_tests_properties(loa_qt_smoke_projects PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_workflows COMMAND loa-qt --smoke workflows)
set_tests_properties(loa_qt_smoke_workflows PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_peers COMMAND loa-qt --smoke peers)
set_tests_properties(loa_qt_smoke_peers PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
add_test(NAME loa_qt_smoke_updates COMMAND loa-qt --smoke updates)
set_tests_properties(loa_qt_smoke_updates PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
