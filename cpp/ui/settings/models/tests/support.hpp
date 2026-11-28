#pragma once

// Shared helpers of the models screen tests.

#include <string_view>
#include <vector>

#include <QApplication>
#include <QObject>

#include "loa/ui/kit/testkit.hpp"

namespace loa::ui::settings::models::test {

// The process-wide application the view tests render into.
QApplication& app();

// Renders `state` into `view`, clicks the control labelled `label` and returns the messages it
// emitted; none when there is no enabled control so labelled.
template <typename View, typename State, typename Message>
std::vector<Message> click(View& view, const State& state, std::string_view label) {
    (void)app();
    std::vector<Message> got;
    view.render(state);
    view.show();
    QApplication::processEvents();
    auto c = QObject::connect(&view, &View::message, &view, [&](const Message& m) { got.push_back(m); });
    (void)loa::ui::kit::testkit::click_label(view, label);
    QObject::disconnect(c);
    return got;
}

} // namespace loa::ui::settings::models::test
