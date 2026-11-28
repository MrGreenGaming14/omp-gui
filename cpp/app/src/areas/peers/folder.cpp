// A peer's shared folder opened on this node: its files listed over `omp/folder/1`, folders
// entered, a file opened in a preview, and with a write grant edited and saved back to the peer
// (port of `areas/peers/folder.rs`).

#include <cstdio>

#include "areas/peers/peers.hpp"
#include "loa/app/perform.hpp"

namespace loa::app::areas::peers {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

// Whether `bytes` are valid UTF-8 (Rust `String::from_utf8`).
bool utf8(const p2p::Bytes& bytes) {
    std::size_t i = 0;
    while (i < bytes.size()) {
        const auto c = bytes[i];
        std::size_t n = 0;
        std::uint32_t cp = 0;
        if (c < 0x80) {
            ++i;
            continue;
        }
        if ((c & 0xE0) == 0xC0) n = 1, cp = c & 0x1F;
        else if ((c & 0xF0) == 0xE0) n = 2, cp = c & 0x0F;
        else if ((c & 0xF8) == 0xF0) n = 3, cp = c & 0x07;
        else return false;
        if (i + n >= bytes.size()) return false;
        for (std::size_t k = 1; k <= n; ++k) {
            if ((bytes[i + k] & 0xC0) != 0x80) return false;
            cp = (cp << 6) | (bytes[i + k] & 0x3F);
        }
        if ((n == 1 && cp < 0x80) || (n == 2 && cp < 0x800) || (n == 3 && cp < 0x10000) || cp > 0x10FFFF ||
            (cp >= 0xD800 && cp <= 0xDFFF))
            return false;
        i += n + 1;
    }
    return true;
}

std::vector<std::string_view> parts(std::string_view path) {
    std::vector<std::string_view> out;
    while (!path.empty()) {
        const auto slash = path.find('/');
        auto part = path.substr(0, slash);
        if (!part.empty()) out.push_back(part);
        if (slash == std::string_view::npos) break;
        path.remove_prefix(slash + 1);
    }
    return out;
}

// Lists `path` of the browser's folder on the peer.
Task list(Browser& browser, std::string path) {
    browser.loading = true;
    return blocking(
        [node = browser.node, peer = browser.peer, folder = browser.folder, path]() mutable {
            auto result = node->list_folder(peer, folder, path);
            std::expected<std::vector<p2p::FolderEntry>, std::string> listed;
            if (result) listed = std::move(*result);
            else listed = std::unexpected(result.error().to_string());
            return folder_msg::Listed{std::move(path), std::move(listed)};
        },
        [](folder_msg::Listed listed) { return State::message(msg::Folder{std::move(listed)}); });
}

} // namespace

std::string join(std::string_view dir, std::string_view name) {
    if (dir.empty()) return std::string(name);
    return std::string(dir) + "/" + std::string(name);
}

std::string size_text(std::uint64_t bytes) {
    char buf[32];
    if (bytes < 1024) return std::to_string(bytes) + " B";
    if (bytes < 1024 * 1024) {
        std::snprintf(buf, sizeof buf, "%.1f KiB", static_cast<double>(bytes) / 1024.0);
        return buf;
    }
    std::snprintf(buf, sizeof buf, "%.1f MiB", static_cast<double>(bytes) / (1024.0 * 1024.0));
    return buf;
}

Task State::folder_update(FolderMessage m, Ctx& ctx) {
    if (!folder_) return Task::none();
    auto& browser = *folder_;
    return std::visit(
        overloaded{
            [&](folder_msg::Enter& e) { return list(browser, join(browser.path, e.name)); },
            [&](folder_msg::Crumb& c) {
                std::string path;
                auto all = parts(browser.path);
                for (std::size_t i = 0; i < all.size() && i < c.depth; ++i) path += (path.empty() ? "" : "/") + std::string(all[i]);
                return list(browser, std::move(path));
            },
            [&](folder_msg::Refresh&) { return list(browser, browser.path); },
            [&](folder_msg::Listed& l) {
                browser.loading = false;
                if (l.result) {
                    browser.path = std::move(l.path);
                    browser.entries = std::move(*l.result);
                } else {
                    ctx.notify(browser.peer_name + " didn't list the folder: " + l.result.error());
                }
                return Task::none();
            },
            [&](folder_msg::Open& o) {
                return blocking(
                    [node = browser.node, peer = browser.peer, folder = browser.folder, path = join(browser.path, o.name)]() mutable {
                        auto result = node->read_folder_file(peer, folder, path);
                        std::expected<p2p::Bytes, std::string> read;
                        if (result) read = std::move(*result);
                        else read = std::unexpected(result.error().to_string());
                        return folder_msg::Opened{std::move(path), std::move(read)};
                    },
                    [](folder_msg::Opened opened) { return message(msg::Folder{std::move(opened)}); });
            },
            [&](folder_msg::Opened& o) {
                if (!o.result) {
                    ctx.notify(browser.peer_name + " didn't open " + o.path + ": " + o.result.error());
                    return Task::none();
                }
                const auto& bytes = *o.result;
                Preview preview = Binary{bytes.size()};
                if (utf8(bytes) && std::ranges::find(bytes, std::uint8_t{0}) == bytes.end())
                    preview = std::string(bytes.begin(), bytes.end());
                browser.file = OpenFile{std::move(o.path), std::move(preview), false, false};
                return Task::none();
            },
            [&](folder_msg::Edit& e) {
                // Without a write grant the preview only scrolls and selects.
                if (!browser.can_write || !browser.file) return Task::none();
                auto* text = std::get_if<std::string>(&browser.file->preview);
                if (!text || *text == e.text) return Task::none();
                *text = std::move(e.text);
                browser.file->dirty = true;
                return Task::none();
            },
            [&](folder_msg::Save&) {
                if (!browser.file) return Task::none();
                auto& file = *browser.file;
                const auto* text = std::get_if<std::string>(&file.preview);
                if (!text || !browser.can_write || file.saving) return Task::none();
                file.saving = true;
                return blocking(
                    [node = browser.node, peer = browser.peer, folder = browser.folder, path = file.path,
                     bytes = p2p::Bytes(text->begin(), text->end())]() mutable {
                        auto result = node->write_folder_file(peer, folder, path, bytes);
                        std::expected<void, std::string> saved;
                        if (!result) saved = std::unexpected(result.error().to_string());
                        return folder_msg::Saved{std::move(path), std::move(saved)};
                    },
                    [](folder_msg::Saved saved) { return message(msg::Folder{std::move(saved)}); });
            },
            [&](folder_msg::Saved& s) {
                if (browser.file && browser.file->path == s.path) {
                    browser.file->saving = false;
                    if (s.result) browser.file->dirty = false;
                }
                if (!s.result) {
                    ctx.notify(browser.peer_name + " didn't save " + s.path + ": " + s.result.error());
                    return Task::none();
                }
                ctx.notify("Saved " + s.path + " on " + browser.peer_name);
                return list(browser, browser.path);
            },
            [&](folder_msg::CloseFile&) {
                browser.file.reset();
                return Task::none();
            },
            [&](folder_msg::Close&) {
                folder_.reset();
                ctx.close_screen();
                return Task::none();
            },
        },
        m);
}

} // namespace loa::app::areas::peers
