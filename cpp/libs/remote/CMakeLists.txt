find_package(OpenSSL 3 REQUIRED COMPONENTS Crypto)
find_package(SQLite3 REQUIRED)
find_package(PkgConfig REQUIRED)
pkg_check_modules(LOA_REMOTE_LIBSSH REQUIRED IMPORTED_TARGET libssh)

# Transport is libssh with ssh-agent authentication only: no code here may call a libssh
# function that loads or decodes private key material (`ssh_pki_import_privkey_*`,
# `ssh_userauth_publickey_auto`, `ssh_userauth_privatekey_file`, ...). `loa_remote_tests`
# greps the sources for them.
add_library(loa_remote STATIC
  src/agent.cpp
  src/audit.cpp
  src/cancel.cpp
  src/config.cpp
  src/connection.cpp
  src/error.cpp
  src/exec.cpp
  src/hosts.cpp
  src/http.cpp
  src/known_hosts.cpp
  src/limits.cpp
  src/ports.cpp
  src/pty.cpp
  src/resolve.cpp
  src/service.cpp
  src/sftp.cpp
  src/tools.cpp
  src/wire.cpp)
add_library(loa::remote ALIAS loa_remote)
target_include_directories(loa_remote PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(loa_remote
  PUBLIC loa::contracts loa::settings loa::runtime
  PRIVATE loa::core loa::mcp PkgConfig::LOA_REMOTE_LIBSSH OpenSSL::Crypto SQLite::SQLite3)

# The tests run OpenSSH `sshd` and `ssh-agent` on loopback (see tests/sshd.hpp) and talk to the
# HTTP endpoint with cpp-httplib.
pkg_check_modules(LOA_REMOTE_HTTPLIB REQUIRED IMPORTED_TARGET cpp-httplib)
loa_add_test(remote
  SOURCES
    tests/audit_test.cpp
    tests/config_test.cpp
    tests/known_hosts_test.cpp
    tests/ports_test.cpp
    tests/remote_test.cpp
    tests/sshd.cpp
    tests/tools_test.cpp
  LINK loa::remote loa::runtime SQLite::SQLite3 OpenSSL::Crypto PkgConfig::LOA_REMOTE_HTTPLIB)
target_compile_definitions(loa_remote_tests PRIVATE
  LOA_GOLDEN_DIR="${CMAKE_SOURCE_DIR}/tests/golden"
  LOA_REMOTE_SRC_DIR="${CMAKE_CURRENT_SOURCE_DIR}/src")
