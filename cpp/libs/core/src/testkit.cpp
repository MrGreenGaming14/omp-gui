#include "loa/core/testkit.hpp"

#include <cstdio>
#include <cstdlib>

namespace loa::core::testkit {

namespace {
[[noreturn]] void fail(const std::string& what) {
    std::fprintf(stderr, "%s\n", what.c_str());
    std::abort();
}
} // namespace

DomFixture DomFixture::new_session() {
    DomFixture f;
    Attrs attrs{{std::string(attr::ID), "s1"}, {std::string(attr::HARNESS), "omp"}, {std::string(attr::NODE), "k"},
                {std::string(attr::CREATED), 1}};
    auto patch = f.dom.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs)), 1);
    if (!patch) fail("fixture session: " + to_string(patch.error()));
    f.session = f.last = patch->node;
    return f;
}

NodeId DomFixture::add(NodeId parent, ElementKind kind, Attrs attrs, std::string_view text) {
    auto patch = dom.commit(Draft::insert(parent, kind, std::move(attrs), std::string(text)), 0);
    if (!patch) fail("fixture insert " + std::string(as_str(kind)) + ": " + to_string(patch.error()));
    return last = patch->node;
}

DomFixture& DomFixture::commit(Draft draft) {
    if (auto patch = dom.commit(std::move(draft), 0); !patch) fail("fixture commit: " + to_string(patch.error()));
    return *this;
}

DomFixture& DomFixture::turn() {
    turn_ = add(session, ElementKind::Turn);
    call_.reset();
    return *this;
}

DomFixture& DomFixture::message(Role role, std::string_view text) {
    add(need_turn(), ElementKind::Message, {{std::string(attr::ROLE), role}}, text);
    return *this;
}

DomFixture& DomFixture::call(std::string_view tool, Attrs attrs) {
    attrs.insert_or_assign(std::string(attr::TOOL), Value(tool));
    const NodeId t = need_turn();
    call_ = add(t, ElementKind::Call, std::move(attrs));
    return *this;
}

DomFixture& DomFixture::decision(DecisionKind kind) {
    const NodeId parent = call_ ? *call_ : need_turn();
    add(parent, ElementKind::Decision, {{std::string(attr::KIND), kind}});
    return *this;
}

DomFixture& DomFixture::job(JobKind kind) {
    add(session, ElementKind::Job, {{std::string(attr::KIND), kind}});
    return *this;
}

NodeId DomFixture::need_turn() {
    if (!turn_) turn();
    return *turn_;
}

} // namespace loa::core::testkit
