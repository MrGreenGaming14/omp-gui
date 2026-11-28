#pragma once

// The library's one set of test fixtures (port of `testing.rs`): DOM builder, in-memory sessions, fake
// summarizer, live sessions.

#include <asio/co_spawn.hpp>
#include <asio/io_context.hpp>
#include <asio/use_future.hpp>
#include <atomic>
#include <filesystem>
#include <fstream>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include "loa/bus/bus.hpp"
#include "loa/bus/store.hpp"
#include "loa/bus/testkit.hpp"
#include "loa/bus/writer.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/projects/digest.hpp"
#include "loa/projects/digester.hpp"
#include "loa/projects/rebuild.hpp"
#include "loa/projects/source.hpp"
#include "loa/runtime/executor.hpp"

namespace loa::projects::test {

namespace attr = core::attr;
using K = core::ElementKind;
using core::Draft;
using core::NodeId;
using core::Value;

inline core::Attrs attrs(std::initializer_list<std::pair<std::string_view, Value>> pairs) {
    core::Attrs out;
    for (const auto& [k, v] : pairs) out.insert_or_assign(std::string(k), v);
    return out;
}

// A session DOM under construction.
struct Builder {
    std::shared_ptr<core::MemorySink> sink = std::make_shared<core::MemorySink>();
    NodeId root;

    Builder(std::string_view id, std::string_view project, std::int64_t created) {
        auto a = attrs({{attr::ID, Value(id)}, {attr::HARNESS, Value("omp")}, {attr::NODE, Value("node-a")},
                        {attr::CREATED, Value(created)}, {attr::PROJECT, Value(project)}});
        root = sink->commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, std::move(a)), created).value().node;
    }

    NodeId commit(Draft draft) const { return sink->commit(std::move(draft)).value().node; }

    // A finished turn with a user prompt and an assistant reply.
    NodeId turn(std::string_view user, std::string_view assistant, std::int64_t ended) const {
        auto t = commit(Draft::insert(root, K::Turn, attrs({{attr::STATUS, Value("done")}, {attr::ENDED, Value(ended)}})));
        commit(Draft::insert(t, K::Message, attrs({{attr::ROLE, Value("user")}}), std::string(user)));
        commit(Draft::insert(t, K::Message, attrs({{attr::ROLE, Value("assistant")}}), std::string(assistant)));
        return t;
    }

    NodeId call(NodeId t, std::string_view tool, std::string_view input, std::string_view result) const {
        auto c = commit(Draft::insert(t, K::Call, attrs({{attr::TOOL, Value(tool)}})));
        commit(Draft::insert(c, K::Input, {}, std::string(input)));
        commit(Draft::insert(c, K::Result, {}, std::string(result)));
        return c;
    }

    void write_digest(const settings::DigestContent& content, std::optional<std::string_view> model = std::nullopt) const {
        const auto d = dom();
        for (auto& draft : write_drafts(d, content, d.seq(), model, 0)) commit(std::move(draft));
    }

    // Writes a digest with `summary`, built from the current DOM.
    void digest(std::string_view summary) const { write_digest(settings::DigestContent{std::string(summary), {}, {}, {}, {}}); }

    core::Dom dom() const { return sink->dom(); }
};

// Sessions in memory, readable and writable.
class MemorySource final : public SessionSource, public SessionWriter {
  public:
    MemorySource() = default;
    explicit MemorySource(std::initializer_list<const Builder*> builders) {
        for (const auto* b : builders) sessions_.insert_or_assign(SessionInfo::from_dom(b->dom())->id, b->sink);
    }

    Result<std::vector<SessionInfo>> list(std::string_view project) const override {
        std::scoped_lock lock(mutex_);
        std::vector<SessionInfo> out;
        for (const auto& [_, sink] : sessions_)
            if (auto info = SessionInfo::from_dom(sink->dom()); info && info->project == project) out.push_back(std::move(*info));
        return out;
    }

    Result<std::shared_ptr<const core::Dom>> load(std::string_view session) const override {
        std::scoped_lock lock(mutex_);
        auto it = sessions_.find(session);
        if (it == sessions_.end()) return std::shared_ptr<const core::Dom>{};
        return std::make_shared<const core::Dom>(it->second->dom());
    }

    Result<core::Patch> commit(std::string_view session, Draft draft) const override {
        std::shared_ptr<core::MemorySink> sink;
        {
            std::scoped_lock lock(mutex_);
            sink = sessions_.find(session)->second;
        }
        auto patch = sink->commit(std::move(draft));
        if (!patch) return std::unexpected(Error::commit(core::to_string(patch.error())));
        return std::move(*patch);
    }

  private:
    mutable std::mutex mutex_;
    std::map<std::string, std::shared_ptr<core::MemorySink>, std::less<>> sessions_;
};

// Shows only the listed sessions.
class Only final : public Access {
  public:
    explicit Only(std::vector<std::string> ids) : ids_(std::move(ids)) {}
    bool can_open(const SessionInfo& session) const override {
        return std::ranges::find(ids_, session.id) != ids_.end();
    }

  private:
    std::vector<std::string> ids_;
};

// A summarizer that answers `digest <n>` citing T1, or fails; it records the models asked for.
class Fake final : public contracts::Summarizer, public Summarizers, public std::enable_shared_from_this<Fake> {
  public:
    explicit Fake(bool fail = false) : fail_(fail) {}

    contracts::Awaitable<std::expected<std::string, std::string>> summarize(std::string, std::string) const override {
        const auto n = calls.fetch_add(1) + 1;
        if (fail_) co_return std::unexpected(std::string("model unavailable"));
        co_return R"({"summary":"digest )" + std::to_string(n) +
            R"(","decisions":[],"files":[],"open_items":[],"pointers":["T1"]})";
    }

    std::pair<std::shared_ptr<const contracts::Summarizer>, std::optional<std::string>> on(
        std::optional<std::string_view> model) const override {
        std::scoped_lock lock(mutex_);
        models.push_back(model ? std::optional<std::string>(std::string(*model)) : std::nullopt);
        return {shared_from_this(), std::string("smol")};
    }

    mutable std::atomic<std::size_t> calls = 0;
    mutable std::vector<std::optional<std::string>> models;

  private:
    bool fail_;
    mutable std::mutex mutex_;
};

// A live session on `bus`, journaled in `store`, created at `at` in `cwd`, in `project` when given.
inline std::shared_ptr<bus::SessionWriter> live(bus::Bus& b, const bus::Store& store, std::string_view id,
                                                std::optional<std::string_view> project, std::string_view cwd, std::int64_t at) {
    auto journal = std::make_shared<bus::JournalWriter>(store.create(id).value(), bus::SyncPolicy{});
    bus::NodeKey key;
    key.bytes.fill(1);
    bus::WriterConfig config{bus::SessionId(id), key, key, core::Dom{}, std::make_unique<bus::JournalPersist>(journal),
                             std::make_shared<bus::testkit::AllowAll>()};
    auto writer = b.open(std::move(config)).value();
    auto a = attrs({{attr::ID, Value(id)}, {attr::HARNESS, Value("omp")}, {attr::NODE, Value("n")},
                    {attr::CREATED, Value(at)}, {attr::CWD, Value(cwd)}});
    if (project) a.insert_or_assign(std::string(attr::PROJECT), Value(*project));
    (void)writer->commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, std::move(a)), at).value();
    return writer;
}

// Runs `aw` on `exec` and blocks the test thread for its result.
template <typename T>
T block_on(runtime::Executor& exec, asio::awaitable<T> aw) {
    return asio::co_spawn(exec.context(), std::move(aw), asio::use_future).get();
}

inline std::string read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

inline void write_file(const std::filesystem::path& path, std::string_view bytes) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
}

} // namespace loa::projects::test
