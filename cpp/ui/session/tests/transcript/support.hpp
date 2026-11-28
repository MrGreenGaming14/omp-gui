#pragma once

// A session DOM and a transcript fed every patch committed to it (the Rust tests' `S`).

#include <doctest/doctest.h>

#include <string>
#include <string_view>
#include <utility>

#include "loa/core/dom.hpp"
#include "loa/ui/session/transcript/rows.hpp"
#include "loa/ui/session/transcript/state.hpp"

namespace transcript_test {

using namespace loa::ui::session::transcript;
namespace core = loa::core;
using core::Attrs;
using core::Draft;
using K = core::ElementKind;

struct S {
    core::Dom dom;
    State chat;
    NodeId session = NodeId::DOCUMENT;

    S() { session = add(NodeId::DOCUMENT, K::Session, {{"id", "s1"}, {"harness", "omp"}, {"node", "n1"}, {"created", 0}}); }
    // A transcript fed only what `commit` sends, with no session yet.
    struct Empty {};
    explicit S(Empty) {}

    NodeId commit(Draft draft) {
        auto patch = dom.commit(std::move(draft), 0);
        REQUIRE(patch.has_value());
        chat.apply(*patch, dom);
        return patch->node;
    }
    NodeId add(NodeId parent, K kind, Attrs attrs = {}, std::string_view text = {}) {
        return commit(Draft::insert(parent, kind, std::move(attrs), std::string(text)));
    }
    NodeId turn() { return add(session, K::Turn); }
    NodeId msg(NodeId turn, std::string_view role, std::string_view text) { return add(turn, K::Message, {{"role", std::string(role)}}, text); }
    NodeId call(NodeId turn, std::string_view tool, std::pair<std::int64_t, std::int64_t> span, std::string_view result) {
        const auto c = add(turn, K::Call, {{"tool", std::string(tool)}, {"status", "done"}, {"started", span.first}, {"ended", span.second}});
        add(c, K::Input, {}, "{}");
        add(c, K::Result, {}, result);
        return c;
    }
    // What drawing does to the state: parses the rows in the window.
    void draw() const {
        const auto w = chat.window();
        for (auto i = w.begin; i < w.end; ++i) (void)chat.context(dom, chat.items()[i], true);
    }
    std::optional<Event> send(Message m) { return chat.update(std::move(m)).second; }
    void scroll(float offset, float viewport) {
        const auto content = static_cast<float>(chat.heights().total());
        send(msg::Scrolled{{offset, viewport, content}});
    }
};

inline S long_session(std::size_t turns) {
    S s;
    for (std::size_t i = 0; i < turns; ++i) {
        const auto t = s.turn();
        s.msg(t, "user", "message " + std::to_string(i));
    }
    return s;
}

} // namespace transcript_test
