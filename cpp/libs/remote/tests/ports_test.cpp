// `PromptQueue`: questions wait for a typed answer and leave when abandoned. Also the private
// key ban.

#include <doctest/doctest.h>

#include <atomic>
#include <filesystem>
#include <fstream>
#include <future>
#include <sstream>

#include "loa/remote/ports.hpp"

using namespace loa::remote;

namespace {

ApprovalRequest request(std::string session) { return ApprovalRequest{std::move(session), "box", Action::command, "uptime", "timeout 120s"}; }

PromptView first(const PromptQueue& queue) {
    auto seen = queue.generation();
    for (int i = 0; i < 200; ++i) {
        auto pending = queue.pending();
        if (!pending.empty()) return pending.front();
        seen = queue.wait_change(seen, std::chrono::milliseconds(50));
    }
    FAIL("no question arrived");
    return {};
}

} // namespace

TEST_CASE("questions wait for a typed answer and leave when abandoned") {
    PromptQueue queue;
    std::atomic<int> ticks{0};
    const auto subscription = queue.subscribe([&](std::uint64_t) { ++ticks; });
    auto asking = std::async(std::launch::async, [&] { return queue.approve(request("s1"), CancelToken()); });
    auto pending = first(queue);
    const auto* r = std::get_if<ApprovalRequest>(&pending.prompt);
    REQUIRE(r);
    CHECK(r->target == "uptime");
    CHECK_MESSAGE(!queue.answer(pending.id, true), "wrong reply type");
    CHECK(queue.answer(pending.id, Approval::allow_session()));
    CHECK(asking.get() == Approval::allow_session());
    CHECK_MESSAGE(!queue.answer(pending.id, Approval::allow_once()), "answered once");
    CHECK(ticks.load() >= 2);

    auto deciding = std::async(std::launch::async, [&] {
        return queue.decide(HostKeyPrompt{"box", "10.0.0.1", 2222, "ssh-ed25519", "SHA256:abc", {}}, CancelToken());
    });
    CHECK(queue.answer(first(queue).id, true));
    CHECK(deciding.get());

    CancelToken cancel;
    auto abandoned = std::async(std::launch::async, [&] { return queue.approve(request("s1"), cancel); });
    first(queue);
    cancel.cancel();
    CHECK(abandoned.get() == Approval::deny("no answer"));
    CHECK_MESSAGE(queue.pending().empty(), "an abandoned call takes its question away");

    auto ended = std::async(std::launch::async, [&] { return queue.approve(request("s2"), CancelToken()); });
    first(queue);
    queue.deny_session("other");
    CHECK(queue.pending().size() == 1);
    queue.deny_session("s2");
    CHECK(ended.get() == Approval::deny("the session ended"));
    CHECK(queue.pending().empty());
}

TEST_CASE("decision answers map to approvals") {
    CHECK(Approval::from_answer("allow_once", std::nullopt) == Approval::allow_once());
    CHECK(Approval::from_answer("allow_session", std::nullopt) == Approval::allow_session());
    CHECK(Approval::from_answer("deny", "no") == Approval::deny("no"));
    CHECK_FALSE(Approval::from_answer("maybe", std::nullopt));
    CHECK(session_of(Prompt{request("s")}) == "s");
    CHECK_FALSE(session_of(Prompt{HostKeyPrompt{}}));
}

TEST_CASE("no source loads private key material") {
    // The Rust crate bans these through clippy.toml; libssh's equivalents stay out of src/.
    const char* banned[] = {"ssh_pki_import_privkey", "ssh_userauth_publickey_auto", "ssh_userauth_privatekey_file", "ssh_userauth_autopubkey",
                            "ssh_pki_export_privkey", "ssh_userauth_publickey(", "privatekey_from_file", "ssh_pki_import_cert_file"};
    for (const auto& entry : std::filesystem::directory_iterator(LOA_REMOTE_SRC_DIR)) {
        std::ifstream in(entry.path());
        std::stringstream text;
        text << in.rdbuf();
        for (const auto* name : banned) {
            CAPTURE(entry.path());
            CAPTURE(name);
            CHECK(text.str().find(name) == std::string::npos);
        }
    }
}
