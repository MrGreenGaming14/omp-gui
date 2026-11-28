// The `App` side of `loa-app/src/app/windows.rs` (window bookkeeping) and `app/media.rs`
// (images and files that reach the composer from outside it).

#include <algorithm>

#include "loa/app/app.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/routes.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::app {

namespace {
template <class... F>
struct overloaded : F... {
    using F::operator()...;
};
} // namespace

Task App::window_update(WindowMessage message) {
    return std::visit(
        overloaded{
            [&](window_msg::OpenConfigs& m) {
                windows.overlay = overlay::Configs{m.tab};
                windows.docs = Docs{scan(m.tab, ctx.runtime.cwd(), loa::runtime::fs::home()), 0, false};
                Task load;
                if (m.tab == ConfigsTab::Models)
                    load = update(area_message(area_id::MODELS, to::models::Show{to::models::Page::Settings}));
                else if (m.tab == ConfigsTab::Agents) load = update(area_message(area_id::AGENTS, to::agents::Open{}));
                // Areas open their own screen when they load; Configs draws them instead.
                ctx.close_screen();
                return load;
            },
            [&](window_msg::PickDoc& m) {
                const auto count = windows.docs.files.size();
                windows.docs.selected = std::min(m.index, count == 0 ? 0 : count - 1);
                return Task::none();
            },
            [&](window_msg::ToggleDocFull&) {
                windows.docs.full = !windows.docs.full;
                return Task::none();
            },
            [&](window_msg::OpenSettings& m) {
                windows.overlay = overlay::Settings{m.tab};
                Task load;
                switch (m.tab) {
                case SettingsTab::Harnesses:
                    load = update(area_message(area_id::MODELS, to::models::Show{to::models::Page::SignIn}));
                    break;
                case SettingsTab::Updates: load = update(area_message(area_id::UPDATER, to::updater::Open{})); break;
                case SettingsTab::Peers: load = update(area_message(area_id::PEERS, to::peers::Refresh{})); break;
                case SettingsTab::General: break;
                }
                ctx.close_screen();
                return load;
            },
            [&](window_msg::CloseOverlay&) {
                windows.overlay.reset();
                return Task::none();
            },
            [&](window_msg::SetTheme& m) {
                return update(msg::Shell{ui::shell::layout::msg::SetTheme{std::move(m.theme)}});
            },
            [&](window_msg::Detach& m) { return detach(detached::Tab{std::move(m.tab)}); },
            [&](window_msg::Show& m) {
                if (auto id = detached_window(detached::Tab{std::move(m.tab)}))
                    return Task::op(ui_op::FocusWindow{*id});
                return Task::none();
            },
            [&](window_msg::ReattachTab& m) {
                if (auto id = detached_window(detached::Tab{std::move(m.tab)}))
                    return window_update(window_msg::Reattach{*id});
                return Task::none();
            },
            [&](window_msg::Reattach& m) {
                auto what = forget_window(m.id);
                if (!what) return Task::none();
                const auto* tab = std::get_if<detached::Tab>(&*what);
                ctx.shell.show_tab(tab ? tab->id : std::string("agents"));
                std::vector<Task> tasks;
                tasks.push_back(Task::op(ui_op::CloseWindow{m.id}));
                tasks.push_back(Task::pane(ui_op::Pane::Target::Shell, ctx.shell.reveal_active_tab()));
                return Task::batch(std::move(tasks));
            },
            // The main window quits the app; a detached one gives its view back to the panel.
            [&](window_msg::Closed& m) {
                if (windows.main == m.id) return Task::op(ui_op::Exit{});
                forget_window(m.id);
                return Task::none();
            },
        },
        message);
}

std::optional<WindowId> App::detached_window(const Detached& what) const {
    for (const auto& [id, shown] : windows.detached)
        if (shown == what) return id;
    return std::nullopt;
}

Task App::detach(Detached what) {
    if (auto id = detached_window(what)) return Task::op(ui_op::FocusWindow{*id});
    const auto id = windows.next++;
    const auto [w, h] = detached_size(what);
    windows.detached.emplace_back(id, std::move(what));
    sync_detached();
    return Task::op(ui_op::OpenWindow{id, w, h});
}

std::optional<Detached> App::forget_window(WindowId id) {
    auto it = std::ranges::find_if(windows.detached, [&](const auto& each) { return each.first == id; });
    if (it == windows.detached.end()) return std::nullopt;
    auto what = std::move(it->second);
    windows.detached.erase(it);
    if (const auto* transcript = std::get_if<detached::Transcript>(&what)) {
        if (auto found = sessions.detached_transcripts.find(transcript->key); found != sessions.detached_transcripts.end())
            sessions.detached_transcripts.erase(found);
    }
    sync_detached();
    return what;
}

void App::sync_detached() {
    std::vector<std::string> tabs;
    for (const auto& [id, what] : windows.detached)
        if (const auto* tab = std::get_if<detached::Tab>(&what)) tabs.push_back(tab->id);
    ctx.shell.set_detached(std::move(tabs));
}

Task App::detach_transcript(std::string key, std::string title, core::Dom dom) {
    auto chat = ui::session::transcript::State::from_dom(dom);
    chat.set_read_only(true);
    sessions.detached_transcripts.insert_or_assign(key, DetachedTranscript{std::move(dom), std::move(chat), std::move(title)});
    return detach(detached::Transcript{std::move(key)});
}

std::string App::detached_title(const Detached& what) const {
    if (const auto* tab = std::get_if<detached::Tab>(&what)) {
        for (const auto& shown : ctx.shell.tabs())
            if (shown.id == tab->id) return shown.title;
        return tab->id;
    }
    const auto& key = std::get<detached::Transcript>(what).key;
    if (auto it = sessions.detached_transcripts.find(key); it != sessions.detached_transcripts.end())
        return it->second.title;
    return "Subagent";
}

// ---- media -----------------------------------------------------------------------------------

Task App::paste_image() {
    if (!read_clipboard_image) return Task::none();
    auto dir = ctx.services.storage.dirs.data / "pasted";
    auto read = read_clipboard_image;
    // The clipboard belongs to the GUI thread: read it now, hand the result back as a message.
    return Task::done(msg::ImagePasted{read(dir)});
}

Task App::image_pasted(std::expected<std::optional<Pasted>, std::string> pasted) {
    if (!pasted) {
        ctx.notify("Can't paste the image: " + pasted.error());
        return Task::none();
    }
    if (!*pasted) return Task::none();
    const auto& image = **pasted;
    return Task::composer(
        ctx.composer.attach_image(ui::session::composer::pasted(image.path, std::pair{image.width, image.height})));
}

Task App::file_dropped(const std::filesystem::path& path) {
    // An image is attached with its card, anything else is mentioned by path.
    if (ui::session::composer::is_image(path)) {
        std::optional<std::pair<std::uint32_t, std::uint32_t>> size;
        if (image_size) size = image_size(path);
        return Task::composer(ctx.composer.attach_image(ui::session::composer::pasted(path, size)));
    }
    return Task::composer(ctx.composer.mention_path(path.string()));
}

} // namespace loa::app
