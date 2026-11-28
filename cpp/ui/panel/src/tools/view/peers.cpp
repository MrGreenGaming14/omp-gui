// The peers section. Port of `crates/loa-ui-panel/src/tools/view/peers.rs`.

#include <QComboBox>

#include "view.hpp"

namespace loa::ui::panel::tools::view {

using namespace pw;

Peers::Peers(const State& s, const Emit& send) {
    setAccessibleName(QStringLiteral("peers"));
    auto* col = box(QBoxLayout::TopToBottom, {}, spi(SpaceRole::S), this);
    std::vector<PeerChoice> choices{PeerChoice::all()};
    for (const auto& p : s.peers()) choices.push_back(PeerChoice::peer(p.key, p.name));
    auto* pick = new QComboBox;
    pick->setAccessibleName(QStringLiteral("peer filter"));
    int current = 0;
    for (std::size_t i = 0; i < choices.size(); ++i) {
        pick->addItem(QString::fromStdString(choices[i].label()));
        if (choices[i].key && choices[i].key == s.peer_filter()) current = static_cast<int>(i);
    }
    pick->setCurrentIndex(current);
    QObject::connect(pick, &QComboBox::activated, pick, [send, choices](int i) {
        if (i >= 0 && static_cast<std::size_t>(i) < choices.size()) send(msg::PeerFilter{choices[static_cast<std::size_t>(i)]});
    });
    auto* header = hrow({}, spi(SpaceRole::S));
    add(header, kw::label("Tools each peer's grant allows", TextRole::Caption, Tone::Muted), 1);
    add(header, pick);
    add(col, header);

    const auto matrix = s.peer_matrix();
    if (matrix.peers.empty()) {
        add(col, card({note_row("No peers")}));
        return;
    }
    const int column_width = px(theme::width(theme::LayoutRole::LabelColumn) / 2.0F);
    auto cell = [&](QLabel* l) {
        l->setFixedWidth(column_width);
        return l;
    };
    auto* head = hrow({}, spi(SpaceRole::S));
    add(head, kw::label("Tool", TextRole::Caption, Tone::Muted), 1);
    for (const auto* p : matrix.peers) add(head, cell(kw::label(p->name, TextRole::Caption, Tone::Muted)));
    head->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
    std::vector<QWidget*> items{head};
    for (const auto& line : matrix.rows) {
        std::string name;
        if (const auto* b = std::get_if<tool_id::Builtin>(&line.tool.value)) name = b->name;
        else {
            const auto& m = std::get<tool_id::Mcp>(line.tool.value);
            name = m.server + " / " + m.tool;
        }
        auto* r = hrow({}, spi(SpaceRole::S));
        add(r, kw::label(name, TextRole::Caption, Tone::Normal), 1);
        for (const bool a : line.allowed) add(r, cell(a ? kw::label("allowed", TextRole::Caption, Tone::Ok) : kw::label("no", TextRole::Caption, Tone::Faint)));
        r->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::Xs), spi(SpaceRole::M), spi(SpaceRole::Xs));
        items.push_back(r);
    }
    if (items.size() == 1) items.push_back(note_row("No tools"));
    add(col, card(items));
}

} // namespace loa::ui::panel::tools::view
