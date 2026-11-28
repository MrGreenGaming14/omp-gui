#include "loa/harness/account_support/opener.hpp"

#include <cerrno>
#include <cstring>
#include <thread>

#if defined(_WIN32)
#include <process.h>
#else
#include <fcntl.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>
extern char** environ;
#endif

namespace loa::harness::account_support {

SystemUrlOpener::Os SystemUrlOpener::current_os() noexcept {
#if defined(__APPLE__)
    return Os::Mac;
#elif defined(_WIN32)
    return Os::Windows;
#else
    return Os::Other;
#endif
}

std::pair<std::string_view, std::vector<std::string>> SystemUrlOpener::command_for(Os os, std::string_view url) {
    switch (os) {
    case Os::Mac: return {"open", {std::string(url)}};
    case Os::Windows: return {"rundll32.exe", {"url.dll,FileProtocolHandler", std::string(url)}};
    case Os::Other: break;
    }
    return {"xdg-open", {std::string(url)}};
}

std::expected<void, std::string> SystemUrlOpener::open_url(std::string_view url) const {
    if (!(url.starts_with("https://") || url.starts_with("http://")))
        return std::unexpected("refusing to open a non-web URL: " + std::string(url));
    auto [program, args] = command_for(current_os(), url);
    std::vector<std::string> argv_text{std::string(program)};
    argv_text.insert(argv_text.end(), args.begin(), args.end());
#if defined(_WIN32)
    // `_spawnvp` passes each element as one argument; no shell is involved.
    std::vector<const char*> argv;
    for (auto& a : argv_text) argv.push_back(a.c_str());
    argv.push_back(nullptr);
    const intptr_t handle = ::_spawnvp(_P_NOWAIT, argv[0], argv.data());
    if (handle == -1) return std::unexpected("cannot start the browser opener: " + std::string(std::strerror(errno)));
    std::thread([handle] { int status = 0; ::_cwait(&status, handle, 0); }).detach();
#else
    std::vector<char*> argv;
    for (auto& a : argv_text) argv.push_back(a.data());
    argv.push_back(nullptr);
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    for (int fd : {0, 1, 2}) posix_spawn_file_actions_addopen(&actions, fd, "/dev/null", fd == 0 ? O_RDONLY : O_WRONLY, 0);
    pid_t pid = -1;
    const int rc = ::posix_spawnp(&pid, argv[0], &actions, nullptr, argv.data(), environ);
    posix_spawn_file_actions_destroy(&actions);
    if (rc != 0)
        return std::unexpected("cannot start the browser opener: " + std::string(std::strerror(rc)) + " (os error " +
                               std::to_string(rc) + ")");
    std::thread([pid] {
        int status = 0;
        while (::waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
    }).detach();
#endif
    return {};
}

} // namespace loa::harness::account_support
