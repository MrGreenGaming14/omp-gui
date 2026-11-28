add_library(loa_session STATIC
  src/error.cpp
  src/hooks.cpp
  src/library.cpp
  src/plan.cpp
  src/session.cpp)
add_library(loa::session ALIAS loa_session)
target_include_directories(loa_session PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_session PUBLIC loa::bus loa::contracts loa::core loa::runtime PRIVATE loa::settings)

loa_add_test(session SOURCES tests/features.cpp LINK loa::session)
