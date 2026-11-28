#include "loa/contracts/session.hpp"

#include <algorithm>
#include <array>
#include <limits>

#include "loa/core/schema.hpp"

namespace loa::contracts {

namespace core = loa::core;
namespace attr = loa::core::attr;

namespace {

// Root attributes the summary copies or derives everything else from.
constexpr std::array<std::string_view, 5> ROOT_ATTRS{attr::TITLE, attr::PROJECT, attr::ID, attr::NODE, attr::CREATED};

constexpr std::size_t DERIVED_TITLE_CHARS = 60;

// Title of a session that has neither a title nor a prompt yet.
constexpr std::string_view UNTITLED = "New session";

// Length of the Unicode `White_Space` character `s` starts with, else 0 (Rust `char::is_whitespace`).
std::size_t space_len(std::string_view s) {
    if (s.empty()) return 0;
    const auto b0 = static_cast<unsigned char>(s[0]);
    if (b0 == ' ' || (b0 >= '\t' && b0 <= '\r')) return 1;
    if (s.size() >= 2 && b0 == 0xC2) {
        const auto b1 = static_cast<unsigned char>(s[1]);
        return b1 == 0x85 || b1 == 0xA0 ? 2 : 0;
    }
    if (s.size() < 3) return 0;
    const auto b1 = static_cast<unsigned char>(s[1]), b2 = static_cast<unsigned char>(s[2]);
    const char32_t c = (char32_t(b0 & 0x0F) << 12) | (char32_t(b1 & 0x3F) << 6) | char32_t(b2 & 0x3F);
    if ((b0 & 0xF0) != 0xE0) return 0;
    const bool space = c == 0x1680 || (c >= 0x2000 && c <= 0x200A) || c == 0x2028 || c == 0x2029 || c == 0x202F ||
                       c == 0x205F || c == 0x3000;
    return space ? 3 : 0;
}

// Length of the white space `s` ends with (whole characters).
std::size_t trailing_space_len(std::string_view s) {
    std::size_t total = 0;
    while (total < s.size()) {
        auto rest = s.substr(0, s.size() - total);
        std::size_t start = rest.size() - 1;
        while (start > 0 && (static_cast<unsigned char>(rest[start]) & 0xC0) == 0x80) --start;
        auto len = space_len(rest.substr(start));
        if (len == 0 || len != rest.size() - start) break;
        total += len;
    }
    return total;
}

std::string_view trim_start(std::string_view s) {
    while (auto n = space_len(s)) s.remove_prefix(n);
    return s;
}

std::string_view trim_end(std::string_view s) { return s.substr(0, s.size() - trailing_space_len(s)); }

std::string_view trim(std::string_view s) { return trim_end(trim_start(s)); }

// `element`'s `ended` time; the minimum without one.
std::int64_t ended_at(const core::Node& element) {
    return element.int_attr(attr::ENDED).value_or(std::numeric_limits<std::int64_t>::min());
}

bool is_waiting(const core::Node& element) {
    return element.kind == core::ElementKind::Decision &&
           element.str_attr(attr::STATUS) == core::as_str(core::DecisionStatus::Waiting);
}

bool is_user_message(const core::Node& element) {
    return element.kind == core::ElementKind::Message && element.str_attr(attr::ROLE) == core::as_str(core::Role::User);
}

// The `title` attribute, else the first line of the first user prompt, else `UNTITLED`.
std::string title_of(const core::Dom& dom, const core::Node& root) {
    if (auto title = root.str_attr(attr::TITLE)) {
        if (auto t = trim(*title); !t.empty()) return std::string(t);
    }
    for (const core::Node& e : dom.iter()) {
        if (!is_user_message(e)) continue;
        return derived_title(e.text).value_or(std::string(UNTITLED));
    }
    return std::string(UNTITLED);
}

std::int64_t latest_update(const core::Dom& dom) {
    const auto* root = dom.root();
    std::int64_t updated = root ? root->int_attr(attr::CREATED).value_or(0) : 0;
    for (const core::Node& e : dom.iter()) updated = std::max(updated, ended_at(e));
    return updated;
}

bool any_waiting(const core::Dom& dom) {
    for (const core::Node& e : dom.iter()) {
        if (is_waiting(e)) return true;
    }
    return false;
}

} // namespace

std::optional<std::string> derived_title(std::string_view prompt) {
    // `str::lines`: split on `\n`, drop one trailing `\r`, no final empty line.
    std::string_view line;
    bool found = false;
    while (!prompt.empty()) {
        auto nl = prompt.find('\n');
        auto raw = prompt.substr(0, nl);
        prompt = nl == std::string_view::npos ? std::string_view{} : prompt.substr(nl + 1);
        if (raw.ends_with('\r')) raw.remove_suffix(1);
        if (auto t = trim(raw); !t.empty()) {
            line = t;
            found = true;
            break;
        }
    }
    if (!found) return std::nullopt;
    std::size_t chars = 0;
    for (std::size_t i = 0; i < line.size(); ++i) {
        if ((static_cast<unsigned char>(line[i]) & 0xC0) == 0x80) continue;
        if (chars++ == DERIVED_TITLE_CHARS) return std::string(trim_end(line.substr(0, i))) + "…";
    }
    return std::string(line);
}

std::optional<SessionSummary> SessionSummary::from_dom(const core::Dom& dom, bool pinned) {
    const auto* root = dom.root();
    if (!root || root->kind != core::ElementKind::Session) return std::nullopt;
    auto id = root->str_attr(attr::ID);
    auto node = root->str_attr(attr::NODE);
    if (!id || !node) return std::nullopt;
    SessionSummary out;
    out.id = std::string(*id);
    out.title = title_of(dom, *root);
    if (auto project = root->str_attr(attr::PROJECT)) out.project = std::string(*project);
    out.node = std::string(*node);
    out.pinned = pinned;
    out.waiting = any_waiting(dom);
    out.updated = latest_update(dom);
    return out;
}

bool SessionSummary::apply(const core::Dom& dom, const core::Patch& patch) {
    const auto* root = dom.root();
    const bool on_root = root && root->id == patch.node;
    const auto* insert = std::get_if<core::op::Insert>(&patch.op);
    const auto* set = std::get_if<core::op::Set>(&patch.op);

    if (std::holds_alternative<core::op::Remove>(patch.op) || (insert && insert->kind == core::ElementKind::Session)) {
        return reload(dom);
    }
    if (set && on_root && std::ranges::find(ROOT_ATTRS, set->name) != ROOT_ATTRS.end()) return reload(dom);

    const auto* node = dom.get(patch.node);
    if (!node) return false;

    if (insert) {
        bool changed = false;
        if (auto time = ended_at(*node); time > updated) {
            updated = time;
            changed = true;
        }
        if (is_waiting(*node) && !waiting) {
            waiting = true;
            changed = true;
        }
        if (is_user_message(*node)) changed |= retitle(dom);
        return changed;
    }
    if (set) {
        if (set->name == attr::ENDED) {
            auto time = node->int_attr(attr::ENDED);
            // The value fell or went away: the latest may be elsewhere.
            const auto next = time && *time >= updated ? *time : latest_update(dom);
            return std::exchange(updated, next) != next;
        }
        if (set->name == attr::STATUS && node->kind == core::ElementKind::Decision) {
            const bool next = any_waiting(dom);
            return std::exchange(waiting, next) != next;
        }
        if ((set->name == attr::ROLE || set->name == core::TEXT) && node->kind == core::ElementKind::Message) {
            return retitle(dom);
        }
        return false;
    }
    // Append.
    return is_user_message(*node) && retitle(dom);
}

bool SessionSummary::reload(const core::Dom& dom) {
    auto summary = from_dom(dom, pinned);
    if (!summary || *summary == *this) return false;
    *this = std::move(*summary);
    return true;
}

bool SessionSummary::retitle(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return false;
    auto title = title_of(dom, *root);
    if (title == this->title) return false;
    this->title = std::move(title);
    return true;
}

} // namespace loa::contracts
