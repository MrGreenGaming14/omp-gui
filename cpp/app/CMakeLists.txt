find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)
find_package(PkgConfig REQUIRED)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  pkg_check_modules(LIBNOTIFY REQUIRED IMPORTED_TARGET libnotify)
endif()

# The app minus `main`, so tests drive the same code as `loa-qt`.
add_library(loa_app STATIC
  # The ports other libraries declare, implemented over this app's libraries (`loa-app/src/adapters`).
  src/adapters/models.cpp
  src/adapters/notifier.cpp
  src/adapters/peer_sessions.cpp
  src/adapters/sandbox.cpp
  src/adapters/ssh_terminal.cpp
  src/adapters/summarizer.cpp
  src/adapters/surface.cpp
  src/adapters/update_gates.cpp
  src/agents_smoke.cpp
  src/app.cpp
  src/app/keys.cpp
  src/app/panes.cpp
  src/app/usage.cpp
  src/areas/workflows/workflows.cpp
  src/app_smoke.cpp
  src/app_windows.cpp
  src/app_workspace.cpp
  src/application.cpp
  src/area.cpp
  src/areas/peers/adapter.cpp
  src/areas/peers/folder.cpp
  src/areas/peers/peers.cpp
  src/areas/peers/remote.cpp
  src/areas/peers/view.cpp
  src/areas.cpp
  src/areas/agents/agents.cpp
  src/areas/projects/projects.cpp
  src/asio_bridge.cpp
  src/decisions_smoke.cpp
  src/host.cpp
  src/kit_gallery.cpp
  src/lifecycle.cpp
  src/local_fs.cpp
  src/models_smoke.cpp
  src/panel_smoke.cpp
  src/peers_updates_smoke.cpp
  src/registry.cpp
  src/services.cpp
  src/settings_smoke.cpp
  src/smoke.cpp
  src/surface_smoke.cpp
  src/terminal_smoke.cpp
  src/transcript_smoke.cpp
  src/watch.cpp
  src/windows.cpp
  src/workspace.cpp
  # Projections between domain state and `loa::view` view models (`loa-app/src/view`).
  src/view/panel.cpp
  src/view/session.cpp
  src/view/surface.cpp
  src/view/theme.cpp
  src/view/settings/agents.cpp
  src/view/settings/models.cpp
  src/view/settings/projects.cpp
  src/view/settings/updates.cpp
  src/view/settings/workflows.cpp)
add_library(loa::app ALIAS loa_app)
target_include_directories(loa_app PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_app PUBLIC loa::ui_kit loa::ui_session loa::ui_shell loa::ui_panel loa::ui_settings loa::ui_terminal loa::ui_surface loa::view loa::runtime
  loa::app_runtime loa::bus loa::contracts loa::harness loa::mcp loa::models loa::p2p loa::projects loa::remote loa::sandbox
  loa::session loa::settings loa::surfaces loa::trust loa::updater Qt6::Core Qt6::Gui Qt6::Widgets)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  # libnotify (and its glib headers) stay private to `notifier.cpp`.
  target_link_libraries(loa_app PRIVATE PkgConfig::LIBNOTIFY)
endif()
# The areas' headers under `src/areas/<area>/` stay private to the app and its tests.
target_include_directories(loa_app PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")

add_executable(loa-qt src/main.cpp)
set_target_properties(loa-qt PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(loa-qt PRIVATE loa::app)

loa_add_test(app SOURCES tests/app_test.cpp tests/app_core_test.cpp tests/view_test.cpp tests/adapters_test.cpp
  tests/update_gates_test.cpp tests/routing_test.cpp tests/support/test_app.cpp tests/glue_test.cpp tests/workflows_test.cpp
  tests/agents_area_test.cpp tests/projects_area_test.cpp tests/peers_test.cpp
  LINK loa::app loa::models_testing loa::harness_testing)
target_include_directories(loa_app_tests PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/tests")
target_compile_definitions(loa_app_tests PRIVATE LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden")
# The area tests reach the areas' private headers under `src/`.
target_include_directories(loa_app_tests PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
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
add_test(NAME loa_qt_smoke_models COMMAND loa-qt --smoke models)
set_tests_properties(loa_qt_smoke_models PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
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
add_test(NAME loa_qt_smoke_decisions COMMAND loa-qt --smoke decisions)
set_tests_properties(loa_qt_smoke_decisions PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen" TIMEOUT 5)
