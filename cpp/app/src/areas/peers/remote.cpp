// A peer's session opened on this node: a live, read-only mirror kept current over
// `omp/session/1` (port of `areas/peers/remote.rs`).

#include "areas/peers/peers.hpp"

#include "loa/app/perform.hpp"
#include "loa/contracts/session.hpp"

namespace loa::app::areas::peers {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

core::Dom mirror(const p2p::RemoteView& view) {
    return view.writer()->with_dom([](const core::Dom& dom) { return dom; });
}

std::expected<void, std::string> plain(p2p::Result<> result) {
    if (!result) return std::unexpected(result.error().to_string());
    return {};
}

// The transcript's clipboard writes; its scroll operations act on the session view only.
Task clipboard_of(const chat::Task& task) {
    chat::Task out;
    for (const auto& op : task.ops)
        if (std::holds_alternative<chat::op::WriteClipboard>(op)) out.ops.push_back(op);
    return Task::chat(std::move(out));
}

} // namespace

Remote::Remote(std::shared_ptr<p2p::RemoteView> view_, std::string peer_name_, std::string by)
    : view(std::move(view_)),
      peer_name(std::move(peer_name_)),
      dom(mirror(*view)),
      cards(cards::Viewer{std::move(by), !view->can(trust::Action::drive)}) {
    chat.load(dom);
    cards.sync(dom);
}

bool Remote::is(std::string_view peer, std::string_view session) const {
    return view->peer().to_string() == peer && view->session() == session;
}

std::string Remote::title() const {
    if (auto summary = contracts::SessionSummary::from_dom(dom, false)) return summary->title;
    return "Session " + view->session();
}

bool Remote::can_drive() const { return !ended && view->can(trust::Action::drive); }

void Remote::reload(core::Dom fresh) {
    dom = std::move(fresh);
    chat.load(dom);
    cards.sync(dom);
}

void Remote::feed(app_runtime::Feed item) {
    std::visit(overloaded{
                   [&](app_runtime::feed::Synced& s) {
                       if (s.dom && s.dom->seq() != dom.seq()) reload(*s.dom);
                   },
                   [&](app_runtime::feed::Events& e) {
                       for (const auto& each : e.events) event(each);
                   },
                   [&](app_runtime::feed::Ended&) {
                       ended = view->ended().value_or("the owner closed the session");
                       cards.set_viewer(cards::Viewer{"", true});
                       cards.sync(dom);
                   },
               },
               item);
}

void Remote::event(const bus::Event& item) {
    const auto* patch = std::get_if<bus::event::Patch>(&item);
    if (!patch || !patch->patch) {
        // Merged appends only reach coalescing feeds, and a lag carries nothing: take the DOM.
        reload(mirror(*view));
        return;
    }
    const auto& p = *patch->patch;
    if (p.seq <= dom.seq()) return;
    if (!dom.apply(p)) {
        // The copy fell out of step: take the mirror's DOM.
        reload(mirror(*view));
        return;
    }
    chat.apply(p, dom);
    if (!std::holds_alternative<core::op::Append>(p.op)) cards.sync(dom);
}

Task State::remote_update(RemoteMessage m, Ctx& ctx) {
    if (!remote_) return Task::none();
    auto& remote = *remote_;
    return std::visit(
        overloaded{
            [&](remote_msg::Feed& f) {
                remote.feed(std::move(f.feed));
                return Task::none();
            },
            [&](remote_msg::Chat& c) {
                auto [task, intent] = remote.chat.update(std::move(c.message));
                if (intent)
                    std::visit(overloaded{
                                   [&](const loa::view::session::transcript::intent::Rewind&) {
                                       ctx.notify("Only the owner rewinds this session; fork it to rewind a copy on this node");
                                   },
                                   [&](const loa::view::session::transcript::intent::OpenLink& l) {
                                       if (!ctx.backends.opener) return;
                                       if (auto r = ctx.backends.opener->open_url(l.url); !r) ctx.notify(r.error());
                                   },
                               },
                               *intent);
                return clipboard_of(task);
            },
            [&](remote_msg::Card& c) {
                // Card keys act only while this screen shows.
                if (std::holds_alternative<cards::msg::Key>(c.message) && ctx.screen() != ID) return Task::none();
                auto [task, intent] = remote.cards.update(std::move(c.message));
                std::vector<Task> tasks;
                tasks.push_back(Task::pane(ui_op::Pane::Target::Shell, task));
                if (intent)
                    std::visit(overloaded{
                                   [&](loa::view::session::decisions::intent::Answer& a) {
                                       auto view = remote.view;
                                       const auto decision = a.decision;
                                       tasks.push_back(blocking(
                                           [view, decision, values = std::move(a.answer.values),
                                            reason = std::move(a.answer.reason)]() mutable {
                                               return plain(view->answer(decision, std::move(values), std::move(reason)));
                                           },
                                           [decision](std::expected<void, std::string> result) {
                                               return message(msg::Remote{remote_msg::Answered{decision, std::move(result)}});
                                           }));
                                   },
                                   [&](loa::view::session::decisions::intent::OpenLink& l) {
                                       if (!ctx.backends.opener) return;
                                       if (auto r = ctx.backends.opener->open_url(l.url); !r) ctx.notify(r.error());
                                   },
                               },
                               *intent);
                return Task::batch(std::move(tasks));
            },
            [&](remote_msg::Answered& a) {
                if (!a.result) {
                    remote.cards.answer_failed(a.decision);
                    ctx.notify(remote.peer_name + " didn't take the answer: " + a.result.error());
                }
                return Task::none();
            },
            [&](remote_msg::Input& i) {
                remote.input = std::move(i.text);
                return Task::none();
            },
            [&](remote_msg::Send&) {
                auto text = remote.input;
                const auto first = text.find_first_not_of(" \t\r\n");
                const auto last = text.find_last_not_of(" \t\r\n");
                text = first == std::string::npos ? std::string{} : text.substr(first, last - first + 1);
                if (text.empty() || remote.sending || !remote.can_drive()) return Task::none();
                remote.sending = true;
                auto view = remote.view;
                return blocking([view, text = std::move(text)] { return plain(view->prompt(text)); },
                                [](std::expected<void, std::string> result) {
                                    return message(msg::Remote{remote_msg::Sent{std::move(result)}});
                                });
            },
            [&](remote_msg::Sent& s) {
                remote.sending = false;
                if (s.result) remote.input.clear();
                else ctx.notify(remote.peer_name + " didn't take the prompt: " + s.result.error());
                return Task::none();
            },
            [&](remote_msg::Fork&) {
                return apply(panel::intent::ForkRemote{remote.view->peer().to_string(), remote.view->session()}, ctx);
            },
            [&](remote_msg::Close&) {
                remote_.reset();
                ctx.close_screen();
                return Task::none();
            },
        },
        m);
}

} // namespace loa::app::areas::peers
