find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets)

# Live video for viewers across the network (Rust feature `remote-video`): the launcher's
# "Live video" section and `panel::msg::Video`.
option(LOA_REMOTE_VIDEO "Build the surface launcher's live video toggle" OFF)

# The surface renderer: pixel tabs (decoder chain onto a retained framebuffer, input in surface
# coordinates, latency meter), document tabs (pages and zoom through the app's renderer), the
# launcher with its file picker, and the runner doing a tab's surface work on the executor. Port
# of `crates/loa-ui-surface`. State code is plain C++; the views are Qt widgets.
add_library(loa_ui_surface STATIC
  src/decode.cpp
  src/geometry.cpp
  src/keys.cpp
  src/latency.cpp
  src/panel.cpp
  src/picker.cpp
  src/runner.cpp
  src/tab.cpp
  src/theme.cpp
  src/view.cpp
  include/loa/ui/surface/view.hpp)
add_library(loa::ui_surface ALIAS loa_ui_surface)
set_target_properties(loa_ui_surface PROPERTIES AUTOMOC ON)
target_include_directories(loa_ui_surface PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_ui_surface PUBLIC loa::ui_kit loa::view loa::contracts loa::runtime Qt6::Core Qt6::Gui Qt6::Widgets)
if(LOA_REMOTE_VIDEO)
  target_compile_definitions(loa_ui_surface PUBLIC LOA_REMOTE_VIDEO)
endif()

loa_add_test(ui_surface
  SOURCES tests/support.cpp tests/units_test.cpp tests/tab_test.cpp tests/panel_test.cpp tests/runner_test.cpp
          tests/view_test.cpp
  LINK loa::ui_surface loa::ui_kit_testkit)
set_tests_properties(loa_ui_surface_tests PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
