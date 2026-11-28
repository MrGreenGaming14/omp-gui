#pragma once

// Chat transcript rendered from the session DOM. `State` is fed every committed patch and keeps
// a cache per row: assistant markdown is parsed the first time its row is drawn, then
// incrementally, so a token append touches one row. The view is a keyed virtual list: only rows
// around the viewport are built, sized by measured heights where known and estimates elsewhere,
// and each row keeps its widgets while the window slides. It follows new content only while
// scrolled to the bottom; opening another session follows its newest rows.
// Port of `crates/loa-ui-session/src/transcript/mod.rs`. Plain C++.

#include <cstddef>
#include <optional>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <variant>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/ui/kit/widgets/virtual_list.hpp"
#include "loa/ui/session/transcript/items.hpp"
#include "loa/ui/session/transcript/prose.hpp"
#include "loa/view/session.hpp"

namespace loa::ui::session::transcript {

using loa::ui::kit::widgets::Heights;
using loa::ui::kit::widgets::Range;

// Pixels of rows built above and below the viewport.
inline constexpr float OVERSCAN = 600.0F;
// Viewport height assumed until the scroll area reports one.
inline constexpr float DEFAULT_VIEWPORT = 1000.0F;
// Upper bound on rows built per frame, whatever their heights.
inline constexpr std::size_t MAX_WINDOW = 200;
// Distance from the end that still counts as "at the bottom".
inline constexpr float BOTTOM_SLACK = 8.0F;
// Measurement changes smaller than this are ignored.
inline constexpr float HEIGHT_EPSILON = 0.5F;
inline constexpr float LINE = 22.0F;
inline constexpr float ROW = 30.0F;
inline constexpr float PADDING = 12.0F;

// Where a decision card goes: the app renders the card for `node` in its place.
struct Slot {
    ItemKind kind = ItemKind::Decision;
    NodeId node;
    friend bool operator==(const Slot&, const Slot&) = default;
};

// The scroll position as the scroll area reports it, in pixels.
struct ScrollMetrics {
    // Distance from the top of the content to the top of the viewport.
    float offset = 0, viewport = 0, content = 0;
    friend bool operator==(const ScrollMetrics&, const ScrollMetrics&) = default;
};

namespace msg {
struct Scrolled {
    ScrollMetrics metrics;
    friend bool operator==(const Scrolled&, const Scrolled&) = default;
};
// A row was laid out at `height`.
struct Measured {
    NodeId key;
    float height = 0;
    friend bool operator==(const Measured&, const Measured&) = default;
};
// Expand or collapse the row starting at this node.
struct Toggle {
    NodeId key;
    friend bool operator==(const Toggle&, const Toggle&) = default;
};
// The rewind action on a user message was clicked.
struct Rewind {
    NodeId node;
    friend bool operator==(const Rewind&, const Rewind&) = default;
};
struct Link {
    std::string url;
    friend bool operator==(const Link&, const Link&) = default;
};
struct Copy {
    std::string text;
    friend bool operator==(const Copy&, const Copy&) = default;
};
struct JumpToLatest {
    friend bool operator==(const JumpToLatest&, const JumpToLatest&) = default;
};
} // namespace msg
using Message = std::variant<msg::Scrolled, msg::Measured, msg::Toggle, msg::Rewind, msg::Link, msg::Copy, msg::JumpToLatest>;

using Event = loa::view::session::transcript::Intent;

// What an update asks the view to do.
namespace op {
// Scroll to the end of the content.
struct SnapToEnd {
    friend bool operator==(const SnapToEnd&, const SnapToEnd&) = default;
};
// Scroll by `y` pixels.
struct ScrollBy {
    float y = 0;
    friend bool operator==(const ScrollBy&, const ScrollBy&) = default;
};
// Put `text` on the clipboard.
struct WriteClipboard {
    std::string text;
    friend bool operator==(const WriteClipboard&, const WriteClipboard&) = default;
};
} // namespace op
using Op = std::variant<op::SnapToEnd, op::ScrollBy, op::WriteClipboard>;

struct Task {
    std::vector<Op> ops;
    [[nodiscard]] static Task none() { return {}; }
    [[nodiscard]] static Task of(Op o) { return Task{{std::move(o)}}; }
    [[nodiscard]] bool empty() const { return ops.empty(); }
    friend bool operator==(const Task&, const Task&) = default;
};

struct RowCtx;

// The transcript component.
class State {
  public:
    State() = default;
    [[nodiscard]] static State from_dom(const loa::core::Dom& dom);

    void set_bottom_inset(float inset) { bottom_inset_ = inset; }
    [[nodiscard]] float bottom_inset() const { return bottom_inset_; }
    void set_read_only(bool read_only) { read_only_ = read_only; }
    [[nodiscard]] bool read_only() const { return read_only_; }

    // True while the view follows new content.
    [[nodiscard]] bool is_following() const { return follow_; }

    // Rebuilds every row from `dom`. The scroll state is kept for the same session; another
    // session starts following its newest rows.
    void load(const loa::core::Dom& dom);
    // Updates the rows after `patch` was applied; `dom` is the state after it.
    void apply(const loa::core::Patch& patch, const loa::core::Dom& dom);
    std::pair<Task, std::optional<Event>> update(Message message);

    // Rows intersecting the viewport plus `OVERSCAN` on each side, at most `MAX_WINDOW`.
    [[nodiscard]] Range window() const;

    [[nodiscard]] const std::vector<Item>& items() const { return items_; }
    [[nodiscard]] const Heights& heights() const { return heights_; }
    [[nodiscard]] std::optional<std::size_t> position(NodeId key) const;
    [[nodiscard]] const ScrollMetrics& metrics() const { return metrics_; }
    [[nodiscard]] bool expanded(NodeId key) const { return expanded_.contains(key); }
    // The parse of an assistant row so far; null while the row was not drawn since its text changed.
    [[nodiscard]] const Streamed* streamed(NodeId key) const;

    // The context of `item`'s row; `parse` fills an assistant row's markdown cache, as drawing
    // does, while estimates use what is parsed already. None when the node is gone.
    [[nodiscard]] std::optional<RowCtx> context(const loa::core::Dom& dom, const Item& item, bool parse) const;

  private:
    struct Span {
        NodeId first;
        std::size_t len = 0;
    };

    std::pair<Task, std::optional<Event>> scrolled(ScrollMetrics metrics);
    Task measure(NodeId key, float height);
    void inserted(const loa::core::Dom& dom, NodeId id);
    [[nodiscard]] bool appends_at_end(const loa::core::Dom& dom, NodeId id, NodeId top) const;
    void emit_rows(const loa::core::Dom& dom, NodeId top, NodeId node, std::vector<Item>& out);
    void append_rows(const loa::core::Dom& dom, NodeId id, NodeId top);
    void restructure(const loa::core::Dom& dom, NodeId top);
    [[nodiscard]] std::optional<Range> span_range(NodeId top) const;
    void rebuild_top(const loa::core::Dom& dom, NodeId top, Range range);
    void prepare_row(std::size_t index);
    void set(const loa::core::Dom& dom, NodeId id, std::string_view name);
    void reestimate(const loa::core::Dom& dom, NodeId id);
    [[nodiscard]] float height_of(const loa::core::Dom& dom, const Item& row) const;
    [[nodiscard]] float estimate(const loa::core::Dom& dom, const Item& row) const;

    std::optional<NodeId> session_;
    std::vector<Item> items_;
    Heights heights_;
    // Row index by row key.
    std::unordered_map<NodeId, std::size_t> pos_;
    // Row key of every node a row shows. Entries of removed nodes are inert: ids never repeat.
    std::unordered_map<NodeId, NodeId> owner_;
    std::unordered_map<NodeId, Span> spans_;
    std::unordered_map<NodeId, float> measured_;
    // Parsed markdown of assistant rows, made the first time a row is drawn.
    mutable std::unordered_map<NodeId, std::optional<Streamed>> streamed_;
    std::unordered_set<NodeId> expanded_;
    ScrollMetrics metrics_;
    // The scroll area has reported a viewport at least once.
    bool known_ = false;
    // Keep the newest content in view.
    bool follow_ = true;
    // `id` of the session the rows show; loading another one starts it following at the bottom.
    std::optional<std::string> shown_;
    // Room under the last row for what floats over the bottom edge (the composer).
    float bottom_inset_ = 0;
    // Suppresses interactive actions like Rewind when rendering read-only transcripts.
    bool read_only_ = false;
};

} // namespace loa::ui::session::transcript
