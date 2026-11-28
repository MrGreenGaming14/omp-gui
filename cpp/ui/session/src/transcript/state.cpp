#include "loa/ui/session/transcript/state.hpp"

#include <algorithm>
#include <cmath>
#include <unordered_set>

#include "loa/ui/session/transcript/rows.hpp"

namespace loa::ui::session::transcript {

namespace core = loa::core;
namespace attr = loa::core::attr;
using core::ElementKind;

State State::from_dom(const core::Dom& dom) {
    State state;
    state.load(dom);
    return state;
}

std::optional<std::size_t> State::position(NodeId key) const {
    const auto it = pos_.find(key);
    if (it == pos_.end()) return std::nullopt;
    return it->second;
}

const Streamed* State::streamed(NodeId key) const {
    const auto it = streamed_.find(key);
    return it == streamed_.end() || !it->second ? nullptr : &*it->second;
}

void State::load(const core::Dom& dom) {
    items_.clear();
    heights_ = Heights{};
    pos_.clear();
    owner_.clear();
    spans_.clear();
    measured_.clear();
    streamed_.clear();
    expanded_.clear();
    const auto* root = dom.root();
    if (root && root->kind != ElementKind::Session) root = nullptr;
    session_ = root ? std::optional(root->id) : std::nullopt;
    const auto shown = root ? root->str_attr(attr::ID) : std::nullopt;
    const auto same = shown ? (shown_ && *shown_ == *shown) : !shown_;
    if (!same) {
        shown_ = shown ? std::optional<std::string>(*shown) : std::nullopt;
        // The old offset belongs to the other session: forget it, so the shorter content it
        // reports next doesn't read as a scroll up.
        follow_ = true;
        known_ = false;
    }
    if (!session_) return;
    for (const auto* top : dom.children(*session_)) {
        const auto at = items_.size();
        rebuild_top(dom, top->id, Range{at, at});
    }
}

void State::apply(const core::Patch& patch, const core::Dom& dom) {
    std::visit(
        [&](const auto& op) {
            using T = std::decay_t<decltype(op)>;
            if constexpr (std::is_same_v<T, core::op::Insert>) {
                if (op.kind == ElementKind::Session) load(dom);
                else inserted(dom, patch.node);
            } else if constexpr (std::is_same_v<T, core::op::Set>) {
                set(dom, patch.node, op.name);
            } else if constexpr (std::is_same_v<T, core::op::Append>) {
                // A row not drawn yet parses the whole text when it is.
                const auto it = streamed_.find(patch.node);
                const auto* node = dom.get(patch.node);
                if (it != streamed_.end() && it->second && node) it->second->append(node->text, op.text);
                reestimate(dom, patch.node);
            } else {
                if (patch.node == session_) return load(dom);
                std::optional<NodeId> top;
                if (spans_.contains(patch.node)) {
                    top = patch.node;
                } else if (const auto o = owner_.find(patch.node); o != owner_.end()) {
                    if (const auto p = pos_.find(o->second); p != pos_.end()) top = items_[p->second].top;
                }
                if (top) restructure(dom, *top);
            }
        },
        patch.op);
}

std::pair<Task, std::optional<Event>> State::update(Message message) {
    namespace intent = loa::view::session::transcript::intent;
    return std::visit(
        [&](auto&& m) -> std::pair<Task, std::optional<Event>> {
            using T = std::decay_t<decltype(m)>;
            if constexpr (std::is_same_v<T, msg::Scrolled>) {
                return scrolled(m.metrics);
            } else if constexpr (std::is_same_v<T, msg::Measured>) {
                return {measure(m.key, m.height), std::nullopt};
            } else if constexpr (std::is_same_v<T, msg::Toggle>) {
                if (!expanded_.erase(m.key)) expanded_.insert(m.key);
                return {Task::none(), std::nullopt};
            } else if constexpr (std::is_same_v<T, msg::Rewind>) {
                return {Task::none(), Event{intent::Rewind{m.node}}};
            } else if constexpr (std::is_same_v<T, msg::Link>) {
                return {Task::none(), Event{intent::OpenLink{std::move(m.url)}}};
            } else if constexpr (std::is_same_v<T, msg::Copy>) {
                return {Task::of(op::WriteClipboard{std::move(m.text)}), std::nullopt};
            } else {
                follow_ = true;
                return {Task::of(op::SnapToEnd{}), std::nullopt};
            }
        },
        std::move(message));
}

std::optional<RowCtx> State::context(const core::Dom& dom, const Item& item, bool parse) const {
    const auto* node = dom.get(item.key);
    if (!node) return std::nullopt;
    const Streamed* streamed = nullptr;
    if (const auto it = streamed_.find(item.key); it != streamed_.end()) {
        if (parse && !it->second) it->second.emplace(node->text);
        if (it->second) streamed = &*it->second;
    }
    return RowCtx{&dom, node, expanded_.contains(item.key), streamed, read_only_};
}

Range State::window() const {
    const auto n = items_.size();
    const double viewport = known_ && metrics_.viewport > 0 ? metrics_.viewport : DEFAULT_VIEWPORT;
    const double bottom = std::max(heights_.total() - viewport, 0.0);
    const double top = follow_ ? bottom : std::clamp(static_cast<double>(metrics_.offset), 0.0, bottom);
    const double overscan = OVERSCAN;
    const auto first = heights_.find(std::max(top - overscan, 0.0));
    const auto last = heights_.find(top + viewport + overscan);
    if (!first || !last) return {0, 0};
    if (follow_ && *last + 1 == n) return {n - std::min(n - *first, MAX_WINDOW), n};
    return {*first, std::min(*last + 1, *first + MAX_WINDOW)};
}

std::pair<Task, std::optional<Event>> State::scrolled(ScrollMetrics metrics) {
    const bool at_bottom = metrics.offset + metrics.viewport >= metrics.content - BOTTOM_SLACK;
    if (at_bottom) {
        follow_ = true;
    } else if (known_ && metrics.offset + HEIGHT_EPSILON < metrics_.offset) {
        // The user scrolled up; content growing under a still viewport keeps following.
        follow_ = false;
    }
    metrics_ = metrics;
    known_ = true;
    return {follow_ && !at_bottom ? Task::of(op::SnapToEnd{}) : Task::none(), std::nullopt};
}

Task State::measure(NodeId key, float height) {
    const auto p = pos_.find(key);
    if (p == pos_.end()) return Task::none();
    const auto index = p->second;
    const auto old = heights_.get(index);
    if (!old) return Task::none();
    measured_[key] = height;
    const float delta = height - *old;
    if (std::abs(delta) < HEIGHT_EPSILON) return Task::none();
    const bool above_viewport = heights_.prefix(index + 1) <= static_cast<double>(metrics_.offset);
    heights_.set(index, height);
    if (follow_ || !above_viewport) return Task::none();
    // A row above the viewport changed size: shift the offset so the reading position stays.
    metrics_.offset += delta;
    metrics_.content += delta;
    return Task::of(op::ScrollBy{delta});
}

void State::inserted(const core::Dom& dom, NodeId id) {
    const auto top = top_of(dom, id);
    if (!top) return;
    if (appends_at_end(dom, id, *top)) append_rows(dom, id, *top);
    else restructure(dom, *top);
}

bool State::appends_at_end(const core::Dom& dom, NodeId id, NodeId top) const {
    auto child = id;
    while (child != top) {
        const auto* node = dom.get(child);
        const auto* parent = node ? dom.get(node->parent) : nullptr;
        if (!parent || parent->children.empty() || parent->children.back() != child) return false;
        child = parent->id;
    }
    if (items_.empty() || items_.back().top == top) return true;
    if (spans_.contains(top)) return false;
    const auto* session = session_ ? dom.get(*session_) : nullptr;
    if (!session) return false;
    for (auto it = session->children.rbegin(); it != session->children.rend() && *it != top; ++it)
        if (spans_.contains(*it)) return false;
    return true;
}

void State::emit_rows(const core::Dom& dom, NodeId top, NodeId node, std::vector<Item>& out) {
    if (const auto* n = dom.get(node)) flatten(dom, top, *n, out, [this](NodeId at, NodeId key) { owner_[at] = key; });
}

void State::append_rows(const core::Dom& dom, NodeId id, NodeId top) {
    const auto start = items_.size();
    const auto last_calls = items_.empty() ? 0 : items_.back().calls.size();
    emit_rows(dom, top, id, items_);
    if (start > 0 && items_[start - 1].calls.size() != last_calls) reestimate(dom, items_[start - 1].key);
    for (auto index = start; index < items_.size(); ++index) {
        prepare_row(index);
        heights_.push(height_of(dom, items_[index]));
        pos_[items_[index].key] = index;
    }
    const auto added = items_.size() - start;
    if (added > 0) {
        if (auto it = spans_.find(top); it != spans_.end()) it->second.len += added;
        else spans_.emplace(top, Span{items_[start].key, added});
    }
}

void State::restructure(const core::Dom& dom, NodeId top) {
    auto range = span_range(top);
    if (!range) {
        std::size_t at = 0;
        if (const auto* session = session_ ? dom.get(*session_) : nullptr) {
            const auto& siblings = session->children;
            auto it = std::find(siblings.rbegin(), siblings.rend(), top);
            if (it != siblings.rend()) ++it;
            for (; it != siblings.rend(); ++it)
                if (const auto r = span_range(*it)) {
                    at = r->end;
                    break;
                }
        }
        range = Range{at, at};
    }
    rebuild_top(dom, top, *range);
}

std::optional<Range> State::span_range(NodeId top) const {
    const auto span = spans_.find(top);
    if (span == spans_.end()) return std::nullopt;
    const auto start = pos_.find(span->second.first);
    if (start == pos_.end()) return std::nullopt;
    return Range{start->second, start->second + span->second.len};
}

void State::rebuild_top(const core::Dom& dom, NodeId top, Range range) {
    std::vector<Item> fresh;
    emit_rows(dom, top, top, fresh);
    std::unordered_set<NodeId> kept;
    for (const auto& r : fresh) kept.insert(r.key);
    for (auto i = range.begin; i < range.end; ++i) {
        const auto key = items_[i].key;
        if (!kept.contains(key)) {
            pos_.erase(key);
            measured_.erase(key);
            streamed_.erase(key);
            expanded_.erase(key);
        }
    }
    const auto start = range.begin, added = fresh.size();
    items_.erase(items_.begin() + static_cast<std::ptrdiff_t>(range.begin), items_.begin() + static_cast<std::ptrdiff_t>(range.end));
    items_.insert(items_.begin() + static_cast<std::ptrdiff_t>(start), std::make_move_iterator(fresh.begin()), std::make_move_iterator(fresh.end()));
    for (auto index = start; index < start + added; ++index) prepare_row(index);
    std::vector<float> heights;
    heights.reserve(added);
    for (auto index = start; index < start + added; ++index) heights.push_back(height_of(dom, items_[index]));
    heights_.splice(range, heights);
    for (auto index = start; index < items_.size(); ++index) pos_[items_[index].key] = index;
    if (added > 0) spans_[top] = Span{items_[start].key, added};
    else spans_.erase(top);
}

void State::prepare_row(std::size_t index) {
    const auto& row = items_[index];
    if (row.kind == ItemKind::Assistant) streamed_.try_emplace(row.key);
    else streamed_.erase(row.key);
}

void State::set(const core::Dom& dom, NodeId id, std::string_view name) {
    const auto* node = dom.get(id);
    if (!node) return;
    const bool structural = (node->kind == ElementKind::Call && name == attr::TOOL) ||
                            (node->kind == ElementKind::Message && (name == attr::ROLE || name == attr::KIND));
    if (structural) {
        if (const auto top = top_of(dom, id)) restructure(dom, *top);
    } else if (name == core::TEXT) {
        // Parsed again when the row is next drawn.
        if (auto it = streamed_.find(id); it != streamed_.end()) it->second.reset();
        reestimate(dom, id);
    }
}

void State::reestimate(const core::Dom& dom, NodeId id) {
    const auto o = owner_.find(id);
    if (o == owner_.end() || measured_.contains(o->second)) return;
    const auto p = pos_.find(o->second);
    if (p == pos_.end()) return;
    heights_.set(p->second, estimate(dom, items_[p->second]));
}

float State::height_of(const core::Dom& dom, const Item& row) const {
    if (const auto it = measured_.find(row.key); it != measured_.end()) return it->second;
    return estimate(dom, row);
}

float State::estimate(const core::Dom& dom, const Item& row) const {
    const auto cx = context(dom, row, false);
    return (cx ? renderer(row.kind).estimate(row, *cx) : ROW) + PADDING;
}

} // namespace loa::ui::session::transcript
