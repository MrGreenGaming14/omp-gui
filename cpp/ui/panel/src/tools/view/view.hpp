#pragma once

// Rendering of the tools page, one class per section; long lists preview their first rows until
// opened. Port of `crates/loa-ui-panel/src/tools/view/mod.rs`. Internal to `loa_ui_panel`.

#include <functional>
#include <map>
#include <string>
#include <vector>

#include "../../pane_widgets.hpp"
#include "loa/ui/panel/tools.hpp"

namespace loa::ui::panel::tools::view {

using Emit = std::function<void(Message)>;

// Lists longer than this show only their first `PREVIEW` rows until opened.
inline constexpr std::size_t COLLAPSE_AT = 8;
inline constexpr std::size_t PREVIEW = 6;
// From this width cards sit in two columns.
inline constexpr int TWO_COLUMNS = 640;

// A callback sending `m`.
[[nodiscard]] inline std::function<void()> on(const Emit& send, Message m) {
    return [send, m] { send(m); };
}

// Rows in a card with a divider between each two.
[[nodiscard]] QWidget* card(const std::vector<QWidget*>& rows);
// A muted line in a card, for empty states and progress.
[[nodiscard]] QWidget* note_row(std::string_view s);
// A full-width row button, disabled without `on`.
[[nodiscard]] QPushButton* row_button(QWidget* content, std::string_view name, std::function<void()> on);
// A coloured dot.
[[nodiscard]] QWidget* status_dot(kit::Tone tone);
[[nodiscard]] kit::Tone detail_tone(DetailState s);
// A primary caption button, disabled without `on`.
[[nodiscard]] QPushButton* primary(std::string_view s, std::function<void()> on);
[[nodiscard]] QWidget* form_buttons(std::string_view submit, FormId form, const Emit& send);
// A form in a card: a bold title, the content, then Submit and Cancel.
[[nodiscard]] QWidget* form_card(std::string_view title, QWidget* trailing, QWidget* content, std::string_view submit, FormId form,
                                 const Emit& send);
// An editor or log opened under a row.
[[nodiscard]] QWidget* inset(QWidget* content);
// Cards in two columns when `wide`, else stacked.
[[nodiscard]] QWidget* grid(const std::vector<QWidget*>& items, bool wide);
// A header that opens and closes its card (not while filtering), with `trailing` actions.
[[nodiscard]] QWidget* block_header(const State& s, const std::string& key, std::string_view title, std::size_t count, QWidget* trailing,
                                    const Emit& send);
// The rows of list `key`, cut to a preview while long, with a show all / fewer row.
[[nodiscard]] std::vector<QWidget*> tool_rows(const State& s, const std::string& key, const std::vector<ToolRow>& tools, const Emit& send);
// Form errors in the kit's key type.
[[nodiscard]] std::map<std::string, std::string, std::less<>> form_errors(const Errors& errors);
// A bold label.
[[nodiscard]] QLabel* bold(std::string_view s, kit::theme::TextRole role);

// The MCP servers section: the add form, the import list and a card per server.
class Servers : public QWidget {
  public:
    Servers(const State& s, const Catalog& catalog, bool wide, const Emit& send);
};

// The remote hosts section: waiting questions, the add form and the hosts with limits editor and audit log.
class Hosts : public QWidget {
  public:
    Hosts(const State& s, bool wide, const Emit& send);
};

// The peers section: which tools each peer's grant allows.
class Peers : public QWidget {
  public:
    Peers(const State& s, const Emit& send);
};

} // namespace loa::ui::panel::tools::view
