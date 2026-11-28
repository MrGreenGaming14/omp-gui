// Port of the tests in `composer/working.rs`.

#include <functional>

#include "loa/core/dom.hpp"
#include "loa/core/testkit.hpp"
#include "support.hpp"

using namespace composer_test;
using loa::core::CallStatus;
using loa::core::Dom;
using CoreDraft = loa::core::Draft;
using loa::core::MessageKind;
using loa::core::MessageStatus;
using loa::core::Role;
using loa::core::TurnStatus;
using loa::core::testkit::DomFixture;
namespace attr = loa::core::attr;

namespace {

using Build = std::function<void(DomFixture&)>;

// A session whose latest turn has `status`, started at 1000, with a user message and then what
// `build` adds.
Dom session(TurnStatus status, const std::vector<Build>& build) {
    auto f = DomFixture::new_session();
    const auto earlier = f.turn().last;
    f.commit(CoreDraft::set(earlier, std::string(attr::STATUS), TurnStatus::Done));
    const auto turn = f.turn().last;
    f.commit(CoreDraft::set(turn, std::string(attr::STATUS), status)).commit(CoreDraft::set(turn, std::string(attr::STARTED), std::int64_t{1000}));
    f.message(Role::User, "hi");
    for (const auto& b : build) b(f);
    return f.build();
}

void streaming(DomFixture& f, MessageKind kind) {
    const auto message = f.message(Role::Assistant, "").last;
    f.commit(CoreDraft::set(message, std::string(attr::KIND), kind)).commit(CoreDraft::set(message, std::string(attr::STATUS), MessageStatus::Streaming));
}

Activity working_(std::string label) { return Activity::working(std::move(label), 1000); }

} // namespace

TEST_CASE("the activity follows the latest turn") {
    const Build reading = [](DomFixture& f) { f.call("read", {{"status", "running"}, {"title", "Reading the config\nand more"}}); };
    const Build bash = [](DomFixture& f) { f.call("bash", {{"status", "running"}}); };
    const Build searched = [](DomFixture& f) { f.call("grep", {{"status", "done"}, {"title", "Searching"}}); };
    const Build thinking = [](DomFixture& f) { streaming(f, MessageKind::Thinking); };
    const Build text = [](DomFixture& f) { streaming(f, MessageKind::Text); };
    const std::tuple<TurnStatus, std::vector<Build>, Activity> cases[] = {
        {TurnStatus::Running, {reading}, working_("Reading the config\u2026")},
        {TurnStatus::Running, {reading, bash}, working_("bash\u2026")},
        {TurnStatus::Running, {thinking}, working_(std::string(working::THINKING))},
        {TurnStatus::Running, {thinking, bash}, working_("bash\u2026")},
        {TurnStatus::Running, {bash, text}, working_(std::string(working::WORKING))},
        {TurnStatus::Running, {searched}, working_(std::string(working::WORKING))},
        {TurnStatus::Running, {}, working_(std::string(working::WORKING))},
        {TurnStatus::Waiting, {bash}, Activity::waiting()},
        {TurnStatus::Done, {thinking}, Activity::idle()},
        {TurnStatus::Cancelled, {bash}, Activity::idle()},
    };
    int n = 0;
    for (const auto& [status, build, expected] : cases) {
        CHECK_MESSAGE(Activity::of(session(status, build)) == expected, "case ", n);
        ++n;
    }
    CHECK(Activity::of(Dom{}) == Activity::idle());
}

TEST_CASE("a long title is cut and ends in one ellipsis") {
    CHECK(working::ongoing("Reading...") == "Reading\u2026");
    const auto long_ = working::ongoing(std::string(100, 'a'));
    std::size_t chars = 0;
    for (const unsigned char c : long_) chars += (c & 0xC0) != 0x80 ? 1 : 0;
    CHECK(chars == working::MAX_TITLE);
    CHECK(long_.ends_with("a\u2026"));
}

TEST_CASE("elapsed time reads in seconds, minutes or hours") {
    const std::pair<std::int64_t, std::string_view> table[] = {{-5, "0s"},         {999, "0s"},          {12'400, "12s"},
                                                               {65'000, "1m 05s"}, {3'599'000, "59m 59s"}, {3'720'000, "1h 02m"}};
    for (const auto& [ms, expected] : table) CHECK_MESSAGE(working::elapsed(ms) == expected, ms);
}

TEST_CASE("the shimmer band brightens the graphemes around its center") {
    const auto levels = [](std::string_view label, float center) {
        std::vector<std::pair<std::string, std::size_t>> out;
        for (const auto& r : working::runs(label, center)) out.emplace_back(std::string(label.substr(r.begin, r.end - r.begin)), r.level);
        return out;
    };
    const std::string_view label = "Working\u2026";
    CHECK_MESSAGE((levels(label, -working::BAND) == std::vector<std::pair<std::string, std::size_t>>{{std::string(label), 0}}),
                  "the band starts off the label");
    CHECK(levels(label, 3.0F) == std::vector<std::pair<std::string, std::size_t>>{{"W", 0}, {"or", 1}, {"k", 2}, {"in", 1}, {"g\u2026", 0}});
    CHECK_MESSAGE((levels("ae\u0301x", 1.0F) == std::vector<std::pair<std::string, std::size_t>>{{"a", 1}, {"e\u0301", 2}, {"x", 1}}),
                  "an accent changes colour with its letter");
}
