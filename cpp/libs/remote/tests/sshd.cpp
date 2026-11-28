#include "sshd.hpp"

#include <fcntl.h>
#include <netinet/in.h>
#include <pwd.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include <chrono>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <stdexcept>
#include <thread>

namespace loa::remote::test {

namespace {

namespace fs = std::filesystem;

std::optional<fs::path> sshd_path() {
    if (const char* env = std::getenv("LOA_SSHD"); env && *env) return fs::path(env);
    if (fs::exists("/usr/sbin/sshd")) return fs::path("/usr/sbin/sshd");
    if (const char* home = std::getenv("HOME")) {
        fs::path cached = fs::path(home) / ".cache/loa/openssh-server/usr/sbin/sshd";
        if (fs::exists(cached)) return cached;
    }
    return std::nullopt;
}

std::optional<fs::path> sftp_server_path() {
    const auto sshd = sshd_path();
    if (!sshd) return std::nullopt;
    for (const auto& candidate : {sshd->parent_path().parent_path() / "lib/openssh/sftp-server", fs::path("/usr/lib/openssh/sftp-server"),
                                  fs::path("/usr/libexec/openssh/sftp-server"), fs::path("/usr/libexec/sftp-server")}) {
        if (fs::exists(candidate)) return candidate;
    }
    return std::nullopt;
}

std::string shell_quote(const std::string& s) {
    std::string out = "'";
    for (const char c : s) out += c == '\'' ? std::string("'\\''") : std::string(1, c);
    return out + "'";
}

// Starts `argv` with stdout and stderr appended to `log`.
pid_t spawn(const std::vector<std::string>& argv, const fs::path& log) {
    const pid_t pid = ::fork();
    if (pid == 0) {
        const int fd = ::open(log.c_str(), O_WRONLY | O_CREAT | O_APPEND, 0600);
        ::dup2(fd, 1);
        ::dup2(fd, 2);
        const int null = ::open("/dev/null", O_RDONLY);
        ::dup2(null, 0);
        std::vector<char*> args;
        for (const auto& a : argv) args.push_back(const_cast<char*>(a.c_str()));
        args.push_back(nullptr);
        ::execv(args[0], args.data());
        ::_exit(127);
    }
    return pid;
}

void stop(pid_t pid) {
    if (pid <= 0) return;
    ::kill(pid, SIGTERM);
    int status = 0;
    ::waitpid(pid, &status, 0);
}

std::uint16_t free_port() {
    const int fd = ::socket(AF_INET, SOCK_STREAM, 0);
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    ::bind(fd, reinterpret_cast<sockaddr*>(&addr), sizeof addr);
    socklen_t len = sizeof addr;
    ::getsockname(fd, reinterpret_cast<sockaddr*>(&addr), &len);
    ::close(fd);
    return ntohs(addr.sin_port);
}

bool accepts(std::uint16_t port) {
    const int fd = ::socket(AF_INET, SOCK_STREAM, 0);
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = htons(port);
    const bool ok = ::connect(fd, reinterpret_cast<sockaddr*>(&addr), sizeof addr) == 0;
    ::close(fd);
    return ok;
}

template <typename F>
void wait_until(F ready, const char* what) {
    for (int i = 0; i < 500; ++i) {
        if (ready()) return;
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    throw std::runtime_error(std::string("timed out waiting for ") + what);
}

std::size_t count(const std::string& haystack, const std::string& needle) {
    std::size_t n = 0;
    for (auto at = haystack.find(needle); at != std::string::npos; at = haystack.find(needle, at + 1)) ++n;
    return n;
}

} // namespace

std::string user() {
    if (const auto* pw = ::getpwuid(::getuid())) return pw->pw_name;
    return std::getenv("USER");
}

bool have_sshd() { return sshd_path() && sftp_server_path(); }

int sh(const std::string& command) {
    const int status = std::system(command.c_str());
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

void keygen(const fs::path& path) {
    if (sh("ssh-keygen -q -t ed25519 -N '' -C test -f " + shell_quote(path.string()) + " </dev/null") != 0) {
        throw std::runtime_error("ssh-keygen failed");
    }
}

std::string slurp(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream out;
    out << in.rdbuf();
    return out.str();
}

PublicKey public_key(const fs::path& pub) {
    auto key = PublicKey::from_openssh(slurp(pub));
    if (!key) throw std::runtime_error("unreadable public key " + pub.string());
    return *key;
}

Agent::Agent(const fs::path& dir) {
    socket = dir / "agent.sock";
    pid_ = spawn({"/usr/bin/ssh-agent", "-D", "-a", socket.string()}, dir / "agent.log");
    wait_until([&] { return fs::exists(socket); }, "ssh-agent");
    keygen(dir / "agent_key");
    if (sh("SSH_AUTH_SOCK=" + shell_quote(socket.string()) + " ssh-add -q " + shell_quote((dir / "agent_key").string()) + " 2>/dev/null") != 0) {
        throw std::runtime_error("ssh-add failed");
    }
    public_line = slurp(dir / "agent_key.pub");
}

Agent::~Agent() { stop(pid_); }

TestServer::TestServer(const std::string& authorized_line) : dir_(*loa::runtime::TempDir::create()) {
    const auto& d = dir_.path();
    keygen(d / "host_key");
    host_key = public_key(d / "host_key.pub");
    {
        std::ofstream keys(d / "authorized_keys");
        keys << authorized_line;
    }
    ::chmod((d / "authorized_keys").c_str(), 0600);
    const auto sftp = sftp_server_path()->string();
    port = free_port();
    {
        std::ofstream cfg(d / "sshd_config");
        cfg << "Port " << port << "\n"
            << "ListenAddress 127.0.0.1\n"
            << "HostKey " << (d / "host_key").string() << "\n"
            << "PidFile " << (d / "sshd.pid").string() << "\n"
            << "AuthorizedKeysFile " << (d / "authorized_keys").string() << "\n"
            << "StrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\n"
            << "AllowTcpForwarding yes\nPermitTTY yes\nMaxStartups 100\nMaxSessions 100\nLogLevel DEBUG1\n"
            << "Subsystem sftp " << sftp << "\n";
    }
    pid_ = spawn({sshd_path()->string(), "-D", "-e", "-f", (d / "sshd_config").string()}, d / "sshd.log");
    wait_until([&] { return log().find("Server listening") != std::string::npos; }, "sshd");
}

TestServer::~TestServer() { stop(pid_); }

std::string TestServer::known_hosts_line() const { return "[127.0.0.1]:" + std::to_string(port) + " " + host_key.to_openssh() + "\n"; }

std::string TestServer::log() const { return slurp(dir_.path() / "sshd.log"); }

std::size_t TestServer::connections() const { return count(log(), "Connection from 127.0.0.1"); }

std::size_t TestServer::commands() const { return count(log(), "Starting session: command for "); }

bool TestServer::forwarded_to(std::uint16_t p) const {
    return log().find("target 127.0.0.1 port " + std::to_string(p)) != std::string::npos;
}

bool TestServer::signalled(const std::string& name) const { return log().find("signal " + name + ", killpg") != std::string::npos; }

bool Decider::decide(HostKeyPrompt prompt, const CancelToken&) {
    std::lock_guard lock(mu_);
    prompts_.push_back(std::move(prompt));
    return accept_;
}

std::vector<HostKeyPrompt> Decider::prompts() const {
    std::lock_guard lock(mu_);
    return prompts_;
}

Approval Scripted::approve(ApprovalRequest request, const CancelToken&) {
    std::lock_guard lock(mu_);
    requests_.push_back(std::move(request));
    if (answers_.empty()) return Approval::deny();
    auto answer = answers_.front();
    answers_.erase(answers_.begin());
    return answer;
}

std::vector<ApprovalRequest> Scripted::requests() const {
    std::lock_guard lock(mu_);
    return requests_;
}

Endpoint endpoint(std::uint16_t port) { return Endpoint{"127.0.0.1", "127.0.0.1", port, user(), AgentSocket{}}; }

AuditRecord Rig::last() const {
    auto row = audit->last();
    if (!row || !*row) throw std::runtime_error("no audit row");
    return **row;
}

HostLimits open_limits() {
    HostLimits l;
    l.allowed_paths = {"/"};
    l.auto_approve_writes = true;
    l.auto_approve_commands = true;
    return l;
}

std::unique_ptr<Rig> Setup::build() const {
    auto rig = std::make_unique<Rig>(Rig{*loa::runtime::TempDir::create(), {}, {}, {}, {}, {}, {}, {}, {}});
    rig->agent = std::make_unique<Agent>(rig->dir.path());
    std::string authorized = rig->agent->public_line;
    if (!this->authorized) {
        keygen(rig->dir.path() / "stranger");
        authorized = slurp(rig->dir.path() / "stranger.pub");
    }
    rig->server = std::make_unique<TestServer>(authorized);
    rig->known_hosts = rig->dir.path() / "known_hosts";
    {
        std::ofstream known_file(rig->known_hosts, std::ios::binary);
        known_file << known.value_or(rig->server->known_hosts_line());
    }
    rig->hosts = std::make_shared<HostRegistry>();
    rig->audit = std::make_shared<MemoryAudit>();
    rig->hosts->insert(Host{"box", endpoint(rig->server->port), {}, limits});
    rig->decider = std::make_shared<Decider>(accept_keys);
    rig->approver = std::make_shared<Scripted>(approvals);
    RemoteServiceConfig config{{rig->known_hosts}, rig->known_hosts,
                               ConnectorConfig::with_agent(agent ? std::optional<fs::path>(rig->agent->socket) : std::nullopt)};
    rig->service = std::make_shared<RemoteService>(rig->hosts, rig->audit, rig->decider, rig->approver, std::move(config));
    return rig;
}

} // namespace loa::remote::test
