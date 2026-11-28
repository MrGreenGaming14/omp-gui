#include <doctest/doctest.h>

#include <httplib.h>

#include <atomic>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <thread>

#include "../kit/support.hpp"
#include "loa/core/schema.hpp"
#include "loa/harness/opencode/json.hpp"
#include "loa/harness/opencode/opencode.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/json.hpp"

namespace {
namespace oc = loa::harness::opencode;
namespace json = loa::runtime::json;
using loa::harness::test::block_on;
using json::Value;
using K = loa::core::ElementKind;

template <typename T>
class Queue {
  public:
    void push(T v) {
        std::lock_guard lock(m_);
        q_.push_back(std::move(v));
        cv_.notify_all();
    }
    std::optional<T> pop(std::chrono::milliseconds timeout) {
        std::unique_lock lock(m_);
        if (!cv_.wait_for(lock, timeout, [&] { return !q_.empty(); })) return std::nullopt;
        T v = std::move(q_.front());
        q_.pop_front();
        return v;
    }

  private:
    std::mutex m_;
    std::condition_variable cv_;
    std::deque<T> q_;
};

// A fake `opencode serve`: events are fed through a queue, POSTs are recorded.
struct Fake {
    httplib::Server server;
    Queue<std::string> events;
    Queue<std::pair<std::string, Value>> requests;
    std::atomic<bool> stopping{false};
    std::atomic<int> streams{0};
    int port = 0;
    std::thread thread;

    Fake() {
        server.Get("/global/health", [](const httplib::Request&, httplib::Response& res) {
            res.set_content(R"({"healthy":true,"version":"1.4.2"})", "application/json");
        });
        server.Get("/config/providers", [](const httplib::Request&, httplib::Response& res) {
            res.set_content(R"({"providers":[{"id":"anthropic","models":{"claude-haiku":{"id":"claude-haiku","name":"Claude Haiku"}}},{"id":"openai","models":{"gpt-mini":{"id":"gpt-mini"}}}]})",
                            "application/json");
        });
        server.Get("/event", [this](const httplib::Request&, httplib::Response& res) {
            ++streams;
            res.set_chunked_content_provider("text/event-stream", [this](std::size_t, httplib::DataSink& sink) {
                while (!stopping && sink.is_writable()) {
                    if (auto data = events.pop(std::chrono::milliseconds(20))) {
                        const std::string frame = "data: " + *data + "\n\n";
                        return sink.write(frame.data(), frame.size());
                    }
                    // A comment line: the write fails once the client has hung up.
                    if (!sink.write(":\n\n", 3)) return false;
                }
                sink.done();
                return true;
            }, [this](bool) { --streams; });
        });
        server.Post(R"(/(.*))", [this](const httplib::Request& req, httplib::Response& res) {
            auto body = req.body.empty() ? Value() : json::parse(req.body).value_or(Value());
            requests.push({req.path.substr(1), std::move(body)});
            res.set_content(R"({"id":"ses_1","time":{"created":100},"directory":"/work"})", "application/json");
        });
        port = server.bind_to_any_port("127.0.0.1");
        thread = std::thread([this] { server.listen_after_bind(); });
        server.wait_until_ready();
    }
    ~Fake() {
        stopping = true;
        server.stop();
        thread.join();
    }
    [[nodiscard]] std::string base() const { return "http://127.0.0.1:" + std::to_string(port); }
    std::pair<std::string, Value> next() {
        auto r = requests.pop(std::chrono::seconds(10));
        REQUIRE(r);
        return std::move(*r);
    }
};

int closed_port() {
    httplib::Server s;
    return s.bind_to_any_port("127.0.0.1");
}
} // namespace

TEST_CASE("opencode session prompts, answers and aborts against a fake server") {
    loa::runtime::Executor exec(2);
    const oc::OpenCodeAdapter down_adapter("http://127.0.0.1:" + std::to_string(closed_port()));
    auto down = block_on(exec, down_adapter.availability());
    CHECK(down.status == loa::contracts::AvailabilityStatus::Unreachable);
    REQUIRE(down.detail);
    CHECK(down.detail->find("opencode serve") != std::string::npos);

    Fake fake;
    const oc::OpenCodeAdapter adapter(fake.base());
    auto up = block_on(exec, adapter.availability());
    CHECK(up.version == std::optional<std::string>("1.4.2"));

    auto sink = std::make_shared<loa::harness::testing::WatchSink>();
    loa::contracts::SessionConfig config;
    config.session_id = "app_1";
    config.cwd = "/work";
    config.model = "p/m";
    auto session = block_on(exec, adapter.start(config, sink));
    REQUIRE_MESSAGE(session.has_value(), (session ? std::string() : session.error().message()));
    CHECK(fake.next().first == "session");

    auto models = block_on(exec, (*session)->models()).value();
    std::vector<std::string> names;
    for (const auto& m : models) names.push_back(m.id + " " + m.name);
    CHECK(names == std::vector<std::string>{"anthropic/claude-haiku Claude Haiku", "openai/gpt-mini gpt-mini"});

    loa::contracts::Prompt prompt;
    prompt.text = "hello";
    prompt.by = "me";
    REQUIRE(block_on(exec, (*session)->send(prompt)));
    auto [path, sent] = fake.next();
    CHECK(path == "session/ses_1/prompt_async");
    CHECK(*oc::json_util::str_at(sent, "/parts/0/text") == "hello");
    CHECK(*sent.find("model") == json::parse(R"({"providerID":"p","modelID":"m"})").value());
    const std::string user = *sent.find("messageID")->as_string();

    const std::string events[] = {
        R"({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":")" + user + R"(","sessionID":"ses_1","role":"user","time":{"created":101}}}})",
        R"({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_u","sessionID":"ses_1","messageID":")" + user + R"(","type":"text","text":"hello"}}})",
        R"({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":"msg_a","sessionID":"ses_1","role":"assistant","parentID":")" + user + R"(","providerID":"p","modelID":"m","time":{"created":101}}}})",
        R"({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_a","sessionID":"ses_1","messageID":"msg_a","type":"text","text":"answer"}}})",
        R"({"type":"permission.asked","properties":{"id":"perm_1","sessionID":"ses_1","permission":"bash","patterns":["*"]}})",
    };
    for (const auto& e : events) fake.events.push(e);
    loa::core::NodeId decision{};
    auto found = sink->until("a decision", [&](const loa::core::Dom& dom) {
        for (const auto& n : dom.iter())
            if (n.kind == K::Decision) {
                decision = n.id;
                return true;
            }
        return false;
    });
    REQUIRE_MESSAGE(found.has_value(), (found ? std::string() : found.error()));
    loa::contracts::Answer answer;
    answer.values = {"allow_once"};
    answer.by = "me";
    REQUIRE(block_on(exec, (*session)->answer(decision, answer)));
    auto reply = fake.next();
    CHECK(reply.first == "permission/perm_1/reply");
    CHECK(reply.second == json::parse(R"({"reply":"once"})").value());

    fake.events.push(R"({"type":"session.idle","properties":{"sessionID":"ses_1"}})");
    REQUIRE(block_on(exec, (*session)->wait_idle()));
    {
        const auto dom = sink->inner.dom();
        auto turns = dom.children_of_kind(dom.root()->id, K::Turn);
        REQUIRE(turns.size() == 1);
        CHECK(turns[0]->str_attr(loa::core::attr::ID) == user);
        CHECK(turns[0]->str_attr(loa::core::attr::STATUS) == "done");
        std::vector<std::pair<std::string, std::string>> texts;
        for (const auto* n : dom.children_of_kind(turns[0]->id, K::Message))
            texts.emplace_back(std::string(n->str_attr(loa::core::attr::ID).value_or("")), n->text);
        CHECK(texts == std::vector<std::pair<std::string, std::string>>{{"prt_u", "hello"}, {"prt_a", "answer"}});
    }

    loa::contracts::Prompt again;
    again.text = "again";
    REQUIRE(block_on(exec, (*session)->send(again)));
    CHECK(fake.next().first == "session/ses_1/prompt_async");
    REQUIRE(block_on(exec, (*session)->interrupt()));
    CHECK(fake.next().first == "session/ses_1/abort");
    {
        const auto dom = sink->inner.dom();
        auto turns = dom.children_of_kind(dom.root()->id, K::Turn);
        CHECK(turns.back()->str_attr(loa::core::attr::STATUS) == "cancelled");
    }

    // Shutting down stops the SSE worker; the server keeps running.
    REQUIRE(block_on(exec, (*session)->shutdown()));
    CHECK((*session)->state() == loa::contracts::SessionState::Closed);
    for (int i = 0; i < 100 && fake.streams > 0; ++i) std::this_thread::sleep_for(std::chrono::milliseconds(20));
    CHECK(fake.streams == 0);
    CHECK(block_on(exec, adapter.availability()).version == std::optional<std::string>("1.4.2"));
}
