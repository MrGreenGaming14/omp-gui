#include "loa/ui/surface/panel.hpp"

#include <algorithm>
#include <cctype>

#include "loa/ui/kit/text.hpp"

namespace loa::ui::surface {

namespace intent = loa::view::surface::intent;

namespace {

std::string trim(std::string_view s) {
    const auto ws = [](char c) { return std::isspace(static_cast<unsigned char>(c)) != 0; };
    while (!s.empty() && ws(s.front())) s.remove_prefix(1);
    while (!s.empty() && ws(s.back())) s.remove_suffix(1);
    return std::string(s);
}

} // namespace

panel::Task panel::Task::of(const SurfaceUri& uri, surface::Task task) {
    Task out;
    out.work.reserve(task.work.size());
    for (auto& w : task.work) out.work.push_back(Routed{uri, std::move(w)});
    return out;
}

Panel::Panel(Options options, BrowserStatus browser, std::filesystem::path documents_dir)
    : options_(std::move(options)), browser_(std::move(browser)),
      picker_(std::move(documents_dir), options_.documents ? options_.documents->extensions() : std::vector<std::string>{}) {}

std::optional<std::size_t> Panel::index(const SurfaceUri& uri) const {
    for (std::size_t i = 0; i < tabs_.size(); ++i)
        if (tabs_[i].state.info().uri == uri) return i;
    return std::nullopt;
}

Intent Panel::start(std::string what, Intent intent) {
    pending_ = std::move(what);
    error_.reset();
    return intent;
}

std::pair<panel::Task, std::optional<Intent>> Panel::update(panel::Message message) {
    namespace m = panel::msg;
    std::optional<Intent> event;
    if (auto* s = std::get_if<m::Surface>(&message)) {
        auto i = index(s->uri);
        if (!i) return {};
        auto [task, ev] = tabs_[*i].state.update(std::move(s->message));
        return {panel::Task::of(s->uri, std::move(task)), std::move(ev)};
    }
    if (auto* s = std::get_if<m::Select>(&message)) {
        if (index(s->uri)) active_ = std::move(s->uri);
    } else if (std::holds_alternative<m::Launcher>(message)) {
        active_.reset();
    } else if (auto* c = std::get_if<m::Close>(&message)) {
        detach(c->uri);
        event = intent::Close{std::move(c->uri)};
    } else if (auto* u = std::get_if<m::Url>(&message)) {
        url_ = std::move(u->url);
    } else if (std::holds_alternative<m::OpenUrl>(message)) {
        std::string url = trim(url_);
        const bool ready = !url.empty() && !std::holds_alternative<loa::view::surface::browser_status::Missing>(browser_);
        if (ready) event = start("Opening " + url, intent::OpenBrowser{url});
    } else if (auto* p = std::get_if<m::Picker>(&message)) {
        if (auto path = picker_.update(std::move(p->message))) event = start("Opening " + path->string(), intent::OpenDocument{*path});
    } else if (auto* a = std::get_if<m::Address>(&message)) {
        address_ = std::move(a->address);
    } else if (auto* p = std::get_if<m::Password>(&message)) {
        password_ = std::move(p->password);
    } else if (std::holds_alternative<m::Connect>(message)) {
        std::string address = trim(address_);
        if (!address.empty()) {
            std::optional<std::string> password;
            if (!password_.empty()) password = std::exchange(password_, {});
            event = start("Connecting to " + address, intent::OpenVm{address, std::move(password)});
        }
    } else if (auto* o = std::get_if<m::OpenAvailable>(&message)) {
        if (index(o->uri)) {
            active_ = std::move(o->uri);
        } else {
            std::string what = "Opening " + o->uri.as_str();
            event = start(std::move(what), intent::Open{std::move(o->uri)});
        }
    }
#if defined(LOA_REMOTE_VIDEO)
    else if (auto* v = std::get_if<m::Video>(&message)) {
        if (v->enabled != video_.enabled) {
            video_.enabled = v->enabled;
            event = intent::SetVideo{v->enabled};
        }
    }
#endif
    else if (auto* c = std::get_if<m::Capture>(&message)) {
        const bool blocked = c->capture == Capture::VirtualDisplay && capture_.virtual_display_missing.has_value();
        if (c->capture != capture_.selected && !blocked) {
            capture_.selected = c->capture;
            event = intent::SetCapture{c->capture};
        }
    }
    return {panel::Task::none(), std::move(event)};
}

panel::Task Panel::attach(SurfacePtr surface, std::string server) {
    SurfaceUri uri = surface->info().uri;
    pending_.reset();
    active_ = uri;
    if (index(uri)) return {};
    auto [state, task] = State::open(std::move(surface), options_);
    std::string label = kit::text::truncate_trailing(state.info().name, TAB_LABEL_CHARS);
    tabs_.push_back(OpenTab{std::move(server), std::move(label), std::move(state)});
    return panel::Task::of(uri, std::move(task));
}

void Panel::detach(const SurfaceUri& uri) {
    auto i = index(uri);
    if (!i) return;
    tabs_.erase(tabs_.begin() + static_cast<std::ptrdiff_t>(*i));
    if (active_ && *active_ == uri) {
        const std::size_t next = *i > 0 ? *i - 1 : 0;
        if (next < tabs_.size()) {
            active_ = tabs_[next].state.info().uri;
        } else {
            active_.reset();
        }
    }
}

void Panel::failed(std::string error) {
    pending_.reset();
    error_ = std::move(error);
    active_.reset();
}

const State* Panel::surface(const SurfaceUri& uri) const {
    auto i = index(uri);
    return i ? &tabs_[*i].state : nullptr;
}

std::vector<const SurfaceInfo*> Panel::open_surfaces() const {
    std::vector<const SurfaceInfo*> out;
    out.reserve(tabs_.size());
    for (const auto& t : tabs_) out.push_back(&t.state.info());
    return out;
}

std::string browser_name(const std::filesystem::path& executable) {
    const std::string path = executable.string();
    if (path.find("ms-playwright/chromium_headless_shell") != std::string::npos) return "Chromium headless shell (Playwright)";
    if (path.find("ms-playwright/chromium") != std::string::npos) return "Chromium (Playwright)";
    if (executable.has_filename()) return executable.filename().string();
    return path;
}

} // namespace loa::ui::surface
