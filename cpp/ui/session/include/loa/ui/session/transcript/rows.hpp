#pragma once

// Row rendering through a `RowRenderer` registry keyed by `ItemKind`, and the summaries shown on
// collapsed rows ("Read 3 files 12s ›"). Port of `crates/loa-ui-session/src/transcript/rows.rs`.
// Qt types are forward-declared; renderers build their widgets in `rows.cpp`.

#include <cstddef>
#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <utility>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/ui/session/transcript/items.hpp"
#include "loa/ui/session/transcript/prose.hpp"
#include "loa/ui/session/transcript/state.hpp"

class QWidget;

namespace loa::ui::session::transcript {

// What a renderer sees of its row.
struct RowCtx {
    const loa::core::Dom* dom = nullptr;
    const loa::core::Node* node = nullptr;
    bool expanded = false;
    const Streamed* streamed = nullptr;
    bool read_only = false;
};

// Reports a row's messages to the view.
using Emit = std::function<void(Message)>;

// Draws and sizes one kind of row.
class RowRenderer {
  public:
    virtual ~RowRenderer() = default;
    // The row's widget; it reports clicks through `post`.
    [[nodiscard]] virtual QWidget* view(const Item& item, const RowCtx& cx, const Emit& post) const = 0;
    // A constant-time height guess before layout, without padding.
    [[nodiscard]] virtual float estimate(const Item& item, const RowCtx& cx) const;
};

// The renderer of `kind`.
[[nodiscard]] const RowRenderer& renderer(ItemKind kind);

// Lines and bytes of a call input, result or job output shown when expanded.
inline constexpr std::size_t MAX_LINES = 40;
inline constexpr std::size_t MAX_BYTES = 6'000;
inline constexpr std::size_t CHARS_PER_LINE = 90;

// How a call group or job is doing.
enum class Status : std::uint8_t { Running, Done, Error, Cancelled };
[[nodiscard]] std::string_view word(Status s);

// What a collapsed call row shows; the duration runs from the first start to the last end.
struct CallSummary {
    std::string label;
    Status status = Status::Running;
    std::optional<std::int64_t> duration_ms;
    std::size_t failed = 0;
    friend bool operator==(const CallSummary&, const CallSummary&) = default;
};
[[nodiscard]] CallSummary summarize(const loa::core::Dom& dom, const std::vector<NodeId>& calls);

// Past-tense verb and nouns of `tool`, Claude Code style: ("Read", "file", "files").
[[nodiscard]] std::tuple<std::string_view, std::string_view, std::string_view> tool_words(std::string_view tool);

// The start of `text`, at most `MAX_LINES` lines and `MAX_BYTES` bytes, and the bytes left out.
[[nodiscard]] std::pair<std::string_view, std::size_t> leading(std::string_view text);
// The end of `text`, bounded like `leading`, and the bytes left out.
[[nodiscard]] std::pair<std::string_view, std::size_t> trailing(std::string_view text);

// The collapsed labels of a thinking note and a compaction.
[[nodiscard]] std::string thinking_label(const loa::core::Node& node);
[[nodiscard]] std::string compaction_label(const loa::core::Node& node);

} // namespace loa::ui::session::transcript
