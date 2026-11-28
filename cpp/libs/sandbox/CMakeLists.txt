option(LOA_SANDBOX_VM "Build the Firecracker and qemu sandbox providers (Rust feature `vm`)" ON)

find_package(LibArchive REQUIRED)

add_library(loa_sandbox STATIC
  src/client.cpp
  src/isolation.cpp
  src/process.cpp
  src/proto.cpp
  src/provider.cpp
  src/providers.cpp
  src/runs.cpp
  src/service.cpp
  src/sync.cpp
  src/tool.cpp
  src/vm.cpp
  src/worker.cpp)
add_library(loa::sandbox ALIAS loa_sandbox)
target_include_directories(loa_sandbox PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_sandbox PUBLIC loa::contracts loa::core loa::runtime PRIVATE LibArchive::LibArchive)
if(LOA_SANDBOX_VM)
  target_compile_definitions(loa_sandbox PUBLIC LOA_SANDBOX_VM)
endif()

# The exec stub: speaks the stub protocol on stdin/stdout. It needs only the wire protocol and
# the process helper, which is all the linker takes from the library.
add_executable(loa-sandbox-stub stub/main.cpp)
target_link_libraries(loa-sandbox-stub PRIVATE loa::sandbox)

loa_add_test(sandbox
  SOURCES
    tests/proto_test.cpp
    tests/sandbox_test.cpp
    tests/units_test.cpp
  LINK loa::sandbox LibArchive::LibArchive)
add_dependencies(loa_sandbox_tests loa-sandbox-stub)
target_compile_definitions(loa_sandbox_tests PRIVATE
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden"
  LOA_SANDBOX_STUB_PATH="$<TARGET_FILE:loa-sandbox-stub>")
