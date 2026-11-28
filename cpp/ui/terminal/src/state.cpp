#include "loa/ui/terminal/state.hpp"

#include <algorithm>
#include <cmath>
#include <limits>

namespace loa::ui::terminal {

// ---- Canvas geometry --------------------------------------------------------------------------

GridSize Metrics::grid_for(float width, float height) const {
    const auto fit = [](float space, float cell) -> std::uint16_t {
        if (cell <= 0) return 0;
        return static_cast<std::uint16_t>(std::clamp(std::floor(space / cell), 0.0F, static_cast<float>(std::numeric_limits<std::uint16_t>::max())));
    };
    return GridSize{std::max<std::uint16_t>(fit(width, cell_width), 2), std::max<std::uint16_t>(fit(height, cell_height), 1),
                    static_cast<std::uint16_t>(std::lround(cell_width)), static_cast<std::uint16_t>(std::lround(cell_height))};
}

std::pair<ViewportCell, Side> Metrics::cell_at(float px, float py, GridSize grid) const {
    const float x = std::max(px / cell_width, 0.0F), y = std::max(py / cell_height, 0.0F);
    const ViewportCell cell{std::min<std::size_t>(static_cast<std::size_t>(x), grid.cols > 0 ? grid.cols - 1U : 0U),
                           std::min<std::size_t>(static_cast<std::size_t>(y), grid.rows > 0 ? grid.rows - 1U : 0U)};
    return {cell, x - std::floor(x) > 0.5F ? Side::Right : Side::Left};
}

// ---- Targets and input --------------------------------------------------------------------------

std::string target_title(const Target& target) {
    if (const auto* remote = std::get_if<Remote>(&target)) return remote->host;
    const auto& cwd = std::get<LocalShell>(target).cwd;
    auto name = cwd.filename();
    if (name.empty()) name = cwd.parent_path().filename();
    return name.empty() ? cwd.string() : name.string();
}

void InputQueue::push(Input item) {
    std::function<void()> wake;
    {
        std::lock_guard lock(mutex_);
        if (closed_) return;
        items_.push_back(std::move(item));
        wake = waker_;
    }
    if (wake) wake();
}

std::optional<Input> InputQueue::try_pop() {
    std::lock_guard lock(mutex_);
    if (items_.empty()) return std::nullopt;
    auto item = std::move(items_.front());
    items_.pop_front();
    return item;
}

void InputQueue::close() {
    std::function<void()> wake;
    {
        std::lock_guard lock(mutex_);
        if (closed_) return;
        closed_ = true;
        wake = waker_;
    }
    if (wake) wake();
}

bool InputQueue::closed() const {
    std::lock_guard lock(mutex_);
    return closed_;
}

void InputQueue::set_waker(std::function<void()> waker) {
    std::lock_guard lock(mutex_);
    waker_ = std::move(waker);
}

// ---- Tabs ---------------------------------------------------------------------------------------

// A sub tab. Destroying it closes `input`, which ends the write loop and hangs up; `opened` is set once its
// channel arrived (input queued before that waits in `input`).
struct State::Tab {
    TabId id;
    Target target;
    std::string title;
    Status status;
    Emulator emulator;
    std::shared_ptr<InputQueue> input = std::make_shared<InputQueue>();
    bool opened = false;

    Tab(TabId i, Target t, GridSize grid) : id(i), target(std::move(t)), title(target_title(target)), emulator(grid) {}
    ~Tab() { input->close(); }
    Tab(const Tab&) = delete;
    Tab& operator=(const Tab&) = delete;

    void send(Input item) const { input->push(std::move(item)); }

    // Messages for this tab; a channel that arrives for a closed tab hangs up as it drops.
    Task update(Message message) {
        if (auto* m = std::get_if<msg::Opened>(&message)) {
            if (opened || !m->channel || !m->channel->reader || !m->channel->writer) return {};
            opened = true;
            status = Status::running();
            auto channel = std::make_shared<PtyChannel>(std::move(*m->channel));
            return {{op::Drive{id, std::move(channel), input}}};
        }
        if (auto* m = std::get_if<msg::Output>(&message)) {
            if (m->read) {
                if (const auto* data = std::get_if<port::channel_read::Data>(&*m->read)) return output(data->bytes);
            } else if (status.kind != Status::Kind::Failed) {
                // The first failure is the one worth reading.
                status = Status::failed(std::move(m->read.error()));
            }
            return {};
        }
        if (auto* m = std::get_if<msg::Canvas>(&message)) return canvas(std::move(m->event));
        if (auto* m = std::get_if<msg::Paste>(&message); m && m->text) {
            send(inputs::Bytes{encode_paste(*m->text, emulator.mode())});
            emulator.scroll_display(Scroll::bottom());
        }
        return {};
    }

    // Feeds output and acts on what the program asked for: replies, titles, clipboard.
    Task output(const Bytes& bytes) {
        emulator.advance(bytes);
        Task task;
        for (auto& e : emulator.take_events()) {
            if (auto* w = std::get_if<event::PtyWrite>(&e)) send(inputs::Bytes{std::move(w->bytes)});
            else if (auto* t = std::get_if<event::Title>(&e)) title = std::move(t->title);
            else if (std::holds_alternative<event::ResetTitle>(e)) title = target_title(target);
            else if (auto* c = std::get_if<event::ClipboardStore>(&e))
                task.ops.emplace_back(op::WriteClipboard{std::move(c->text), c->type == ClipboardType::Selection});
        }
        return task;
    }

    Task canvas(CanvasEvent event) {
        const TermMode mode = emulator.mode();
        if (auto* k = std::get_if<canvas_event::KeyPress>(&event)) {
            const auto typed = k->text ? std::optional<std::string_view>(*k->text) : std::nullopt;
            const auto action = key_action(k->key, k->modifiers, typed, mode);
            if (!action) return {};
            switch (action->kind) {
            case KeyAction::Kind::Send:
                send(inputs::Bytes{action->bytes});
                emulator.scroll_display(Scroll::bottom());
                break;
            case KeyAction::Kind::Copy:
                if (auto text = emulator.selection_text()) return {{op::WriteClipboard{std::move(*text), false}}};
                break;
            case KeyAction::Kind::Paste: return {{op::ReadClipboard{id}}};
            case KeyAction::Kind::Scroll: emulator.scroll_display(action->scroll); break;
            }
        } else if (auto* s = std::get_if<canvas_event::Scroll>(&event)) {
            // Full screen programs (less, vim) get arrow keys for the wheel.
            if (mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !mode.intersects(TermMode::MOUSE_MODE)) {
                const Bytes key{0x1b, static_cast<std::uint8_t>(mode.contains(TermMode::APP_CURSOR) ? 'O' : '['),
                                static_cast<std::uint8_t>(s->lines > 0 ? 'A' : 'B')};
                Bytes keys;
                for (int i = 0; i < std::abs(s->lines); ++i) keys.insert(keys.end(), key.begin(), key.end());
                send(inputs::Bytes{std::move(keys)});
            } else {
                emulator.scroll_display(Scroll::lines(s->lines));
            }
        } else if (auto* st = std::get_if<canvas_event::SelectStart>(&event)) {
            emulator.select_start(st->kind, st->cell, st->side);
        } else if (auto* up = std::get_if<canvas_event::SelectUpdate>(&event)) {
            emulator.select_update(up->cell, up->side);
        } else if (std::holds_alternative<canvas_event::SelectEnd>(event)) {
            if (auto text = emulator.selection_text()) return {{op::WriteClipboard{std::move(*text), true}}};
        }
        return {};
    }
};

State::State(Metrics metrics, LocalShell new_tab)
    : metrics_(metrics), new_tab_(std::move(new_tab)), backend_(std::make_shared<LocalPtyBackend>()) {}
State::~State() = default;
State::State(State&&) noexcept = default;
State& State::operator=(State&&) noexcept = default;

void State::with_remote(std::shared_ptr<const port::RemotePty> remote) {
    backend_ = std::make_shared<RemotePtyBackend>(std::move(remote), backend_);
}

std::pair<TabId, Task> State::open(Target target) {
    const TabId id{next_id_++};
    tabs_.push_back(std::make_unique<Tab>(id, target, grid_));
    active_ = id;
    return {id, Task{{op::Open{id, std::move(target), grid_}}}};
}

std::vector<TabId> State::tabs() const {
    std::vector<TabId> out;
    out.reserve(tabs_.size());
    for (const auto& t : tabs_) out.push_back(t->id);
    return out;
}

State::Tab* State::tab(TabId id) const {
    const auto it = std::ranges::find_if(tabs_, [id](const auto& t) { return t->id == id; });
    return it == tabs_.end() ? nullptr : it->get();
}

std::optional<std::string_view> State::title(TabId id) const {
    if (const auto* t = tab(id)) return t->title;
    return std::nullopt;
}
const Status* State::status(TabId id) const {
    const auto* t = tab(id);
    return t ? &t->status : nullptr;
}
const Target* State::target(TabId id) const {
    const auto* t = tab(id);
    return t ? &t->target : nullptr;
}
const Emulator* State::emulator(TabId id) const {
    const auto* t = tab(id);
    return t ? &t->emulator : nullptr;
}
std::shared_ptr<InputQueue> State::input(TabId id) const {
    const auto* t = tab(id);
    return t ? t->input : nullptr;
}

// Shows a sub tab, bringing its grid to the pane's size.
void State::show(TabId id) {
    auto* t = tab(id);
    if (!t) return;
    if (t->emulator.resize(grid_)) t->send(inputs::Resize{grid_});
    active_ = id;
}

Task State::update(Message message) {
    if (std::holds_alternative<msg::NewTab>(message)) return open(Target{new_tab_}).second;
    if (const auto* m = std::get_if<msg::Select>(&message)) {
        show(m->tab);
        return {};
    }
    if (const auto* m = std::get_if<msg::Canvas>(&message)) {
        if (const auto* r = std::get_if<canvas_event::Resized>(&m->event)) {
            grid_ = r->grid;
            show(m->tab);
            return {};
        }
    }
    std::optional<TabId> closing;
    if (const auto* m = std::get_if<msg::Close>(&message)) closing = m->tab;
    if (const auto* m = std::get_if<msg::Output>(&message); m && m->read && std::holds_alternative<port::channel_read::Closed>(*m->read))
        closing = m->tab;
    if (closing) {
        // Dropping the tab hangs up its program; its neighbour becomes active.
        const auto it = std::ranges::find_if(tabs_, [&](const auto& t) { return t->id == *closing; });
        if (it == tabs_.end()) return {};
        const auto index = static_cast<std::size_t>(it - tabs_.begin());
        tabs_.erase(it);
        if (active_ == closing) {
            active_.reset();
            if (index < tabs_.size()) active_ = tabs_[index]->id;
            else if (!tabs_.empty()) active_ = tabs_.back()->id;
            if (active_) show(*active_);
        }
        return {};
    }
    const TabId id = std::visit(
        [](const auto& m) -> TabId {
            if constexpr (requires { m.tab; }) return m.tab;
            else return {};
        },
        message);
    auto* t = tab(id);
    return t ? t->update(std::move(message)) : Task{};
}

} // namespace loa::ui::terminal
