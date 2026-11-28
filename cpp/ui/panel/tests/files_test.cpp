// Port of the `files.rs` tests: lazy listing, ordering, opening, refresh.

#include <doctest/doctest.h>

#include <map>

#include "loa/ui/panel/files.hpp"

using namespace loa::ui::panel::files;
namespace fi = loa::view::panel::files_intent;

namespace {

// Directories by path, counting how often each is listed.
struct FakeFs : FileSystem {
    mutable std::map<Path, std::pair<std::vector<Entry>, std::size_t>> dirs;
    void put(const std::string& dir, std::vector<Entry> entries) { dirs[dir].first = std::move(entries); }
    [[nodiscard]] std::size_t calls(const std::string& dir) const {
        const auto it = dirs.find(dir);
        return it == dirs.end() ? 0 : it->second.second;
    }
    std::expected<std::vector<Entry>, std::string> list(const Path& dir) const override {
        const auto it = dirs.find(dir);
        if (it == dirs.end()) return std::unexpected(std::string("entity not found"));
        ++it->second.second;
        return it->second.first;
    }
};

std::vector<std::string> visible(const State& s) {
    std::vector<std::string> out;
    for (const auto& p : s.visible()) out.push_back(p.generic_string());
    return out;
}

std::optional<Event> click(State& s, const std::string& rel) {
    for (std::size_t i = 0; i < s.nodes().size(); ++i)
        if (s.nodes()[i].rel == Path(rel)) return s.update(msg::Click{i}).second;
    FAIL("no node " << rel);
    return std::nullopt;
}

} // namespace

TEST_CASE("directories list lazily, sort first and open files") {
    auto fs = std::make_shared<FakeFs>();
    fs->put("/r", {{"b.txt", false}, {"src", true}, {"a", true}, {"Z.md", false}});
    fs->put("/r/src", {{"main.rs", false}, {"lib", true}});
    fs->put("/r/a", {});
    State s("/r", fs);
    CHECK(visible(s) == std::vector<std::string>{"a", "src", "b.txt", "Z.md"});
    CHECK(fs->calls("/r") == 1);
    CHECK(fs->calls("/r/src") == 0);
    struct Step {
        std::string rel;
        std::optional<Event> event;
        std::vector<std::string> want;
    };
    const std::vector<Step> steps{
        {"src", std::nullopt, {"a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"}},
        {"src", std::nullopt, {"a", "src", "b.txt", "Z.md"}},
        {"src", std::nullopt, {"a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"}},
        {"src/main.rs", Event{fi::Open{"/r/src/main.rs"}}, {"a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"}},
    };
    for (const auto& st : steps) {
        CAPTURE(st.rel);
        CHECK(click(s, st.rel) == st.event);
        CHECK(visible(s) == st.want);
        CHECK(fs->calls("/r/src") == 1);
    }
    fs->put("/r/src", {{"main.rs", false}, {"new.rs", false}});
    s.refresh();
    CHECK(visible(s).size() == 6);
    REQUIRE(s.selected());
    CHECK(s.nodes()[*s.selected()].rel == Path("src/main.rs"));
}

TEST_CASE("a listing error shows under its directory") {
    auto fs = std::make_shared<FakeFs>();
    fs->put("/r", {{"gone", true}});
    State s("/r", fs);
    (void)click(s, "gone");
    REQUIRE(s.rows().size() == 2);
    CHECK(s.rows()[1] == Row{Row::Kind::Error, 1});
    CHECK(s.nodes()[1].error == std::optional<std::string>("entity not found"));
    s.set_root("/r");
    CHECK(visible(s) == std::vector<std::string>{"gone"});
    CHECK_FALSE(s.selected());
}
