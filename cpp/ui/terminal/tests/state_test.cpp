// The terminal state (port of `src/state/tests.rs`), the grid geometry of `canvas.rs` and the colours of
// `palette.rs`.

#include <doctest/doctest.h>

#include <filesystem>
#include <mutex>
#include <string>

#include <asio.hpp>

#include "loa/ui/kit/theme.hpp"
#include "loa/ui/terminal/palette.hpp"
#include "loa/ui/terminal/runner.hpp"
#include "loa/ui/terminal/state.hpp"

using namespace loa::ui::terminal;

namespace {

constexpr TabId ID{1};

State state() { return State(Metrics{14, 8, 16}, LocalShell::login_shell(std::filesystem::temp_directory_path())); }
Target remote(std::string host) { return Remote{std::move(host)}; }
Bytes b(std::string_view s) { return {s.begin(), s.end()}; }
Message data(TabId id, std::string_view s) { return msg::Output{id, ChannelRead{port::channel_read::Data{b(s)}}}; }
Message key(std::string k, Modifiers m) { return msg::Canvas{ID, canvas_event::KeyPress{Key::character(k), m, k}}; }
Message canvas(CanvasEvent e) { return msg::Canvas{ID, std::move(e)}; }
Input bytes(std::string_view s) { return inputs::Bytes{b(s)}; }

struct Case {
    std::vector<Message> messages;
    std::vector<Input> sent;
    std::vector<std::string> clipboard;
};

} // namespace

// Input and clipboard writes per message sequence, on a tab whose channel never opens so its input stays queued.
TEST_CASE("messages queue input and write the clipboard") {
#if defined(__APPLE__)
    const Modifiers clip{Modifiers::LOGO};
#else
    const Modifiers clip{Modifiers::CTRL | Modifiers::SHIFT};
#endif
    const auto start = canvas(canvas_event::SelectStart{{0, 0}, Side::Left, SelectionType::Simple});
    const auto update = canvas(canvas_event::SelectUpdate{{4, 0}, Side::Right});
    const Case cases[] = {
        {{key("l", {}), key("c", {Modifiers::CTRL})}, {bytes("l"), bytes("\x03")}, {}},
        {{data(ID, "ab\x1b[6n")}, {bytes("\x1b[1;3R")}, {}},
        {{canvas(canvas_event::Resized{GridSize{60, 20}})}, {inputs::Resize{GridSize{60, 20}}}, {}},
        {{data(ID, "\x1b[?1049h"), canvas(canvas_event::Scroll{2})}, {bytes("\x1b[A\x1b[A")}, {}},
        {{data(ID, "\x1b[?2004h"), msg::Paste{ID, "make\x1b\ntest"}}, {bytes("\x1b[200~make\ntest\x1b[201~")}, {}},
        {{msg::Paste{ID, "a\nb\r\nc"}}, {bytes("a\rb\rc")}, {}},
        {{data(ID, "hello world"), start, update, canvas(canvas_event::SelectEnd{}), key("C", clip)}, {}, {"hello", "hello"}},
        {{data(ID, "\x1b]52;c;aGk=\x07")}, {}, {"hi"}},
    };
    for (const auto& c : cases) {
        auto s = state();
        std::vector<std::string> clipped;
        (void)s.open(remote("h"));
        for (const auto& m : c.messages) {
            const auto task = s.update(m);
            for (const auto& o : task.ops)
                if (const auto* w = std::get_if<op::WriteClipboard>(&o)) clipped.push_back(w->text);
        }
        std::vector<Input> sent;
        const auto queue = s.input(ID);
        REQUIRE(queue);
        while (auto item = queue->try_pop()) sent.push_back(std::move(*item));
        CHECK(sent == c.sent);
        CHECK(clipped == c.clipboard);
    }
}

TEST_CASE("paste reads the clipboard through a message") {
    auto s = state();
    (void)s.open(remote("h"));
#if defined(__APPLE__)
    const Modifiers clip{Modifiers::LOGO};
#else
    const Modifiers clip{Modifiers::CTRL | Modifiers::SHIFT};
#endif
    const auto task = s.update(key("V", clip));
    REQUIRE(task.ops.size() == 1);
    const auto* read = std::get_if<op::ReadClipboard>(&task.ops[0]);
    REQUIRE(read);
    CHECK(read->tab == ID);
    (void)s.update(msg::Paste{ID, "ls"});
    (void)s.update(msg::Paste{ID, std::nullopt});
    std::vector<Input> sent;
    while (auto item = s.input(ID)->try_pop()) sent.push_back(std::move(*item));
    CHECK(sent == std::vector<Input>{bytes("ls")});
}

TEST_CASE("tabs follow titles, selection, closing and failures") {
    auto s = state();
    const auto a = s.open(remote("a")).first;
    const auto bt = s.open(remote("b")).first;
    CHECK(*s.status(bt) == Status::connecting());
    for (Message m : std::vector<Message>{data(bt, "\x1b]0;vim\x07last words"), msg::Select{a}, msg::Close{a},
                                          msg::Output{bt, std::unexpected(std::string("broken pipe"))},
                                          msg::Output{bt, std::unexpected(std::string("later"))}})
        (void)s.update(std::move(m));
    CHECK(s.tabs() == std::vector<TabId>{bt});
    CHECK(s.active() == bt);
    CHECK(s.title(bt) == std::optional<std::string_view>("vim"));
    CHECK(*s.status(bt) == Status::failed("broken pipe"));
    CHECK_MESSAGE(s.emulator(bt)->screen_text().find("last words") != std::string::npos, "a failure keeps the screen");
    (void)s.update(msg::Output{bt, ChannelRead{port::channel_read::Closed{0}}});
    CHECK((s.is_empty() && !s.active()));
}

TEST_CASE("an opened channel runs the tab and a closed tab hangs it up") {
    auto s = state();
    const auto id = s.open(remote("a")).first;
    struct Nop final : port::ChannelReader, port::ChannelWriter {
        loa::contracts::Awaitable<port::Result<ChannelRead>> read() override { co_return ChannelRead{port::channel_read::Closed{}}; }
        loa::contracts::Awaitable<port::Result<>> write(std::vector<std::uint8_t>) override { co_return port::Result<>{}; }
        loa::contracts::Awaitable<port::Result<>> resize(GridSize) override { co_return port::Result<>{}; }
        loa::contracts::Awaitable<port::Result<>> close() override { co_return port::Result<>{}; }
    };
    auto channel = std::make_shared<PtyChannel>(PtyChannel{std::make_unique<Nop>(), std::make_unique<Nop>()});
    const auto task = s.update(msg::Opened{id, channel});
    CHECK(*s.status(id) == Status::running());
    REQUIRE(task.ops.size() == 1);
    const auto* drive = std::get_if<op::Drive>(&task.ops[0]);
    REQUIRE(drive);
    CHECK(drive->channel->reader);
    CHECK_FALSE(channel->reader);
    // The channel is taken once.
    CHECK(s.update(msg::Opened{id, channel}).empty());
    (void)s.update(msg::Close{id});
    CHECK(drive->input->closed());
}

namespace {

// Hands out one channel, then refuses.
struct Host final : port::RemotePty {
    mutable std::mutex mutex;
    mutable std::optional<PtyChannel> channel;
    loa::contracts::Awaitable<port::Result<PtyChannel>> open_pty(std::string host, GridSize) const override {
        std::lock_guard lock(mutex);
        if (!channel) co_return std::unexpected(port::Error::remote(host, "refused"));
        auto out = std::move(*channel);
        channel.reset();
        co_return out;
    }
};

// Reads queued output, one item per read.
struct Script final : port::ChannelReader {
    std::vector<ChannelRead> reads;
    loa::contracts::Awaitable<port::Result<ChannelRead>> read() override {
        auto next = reads.front();
        reads.erase(reads.begin());
        co_return next;
    }
};

// The writer side the test reads back: `Some` bytes, `None` on close.
struct Sent final : port::ChannelWriter {
    std::shared_ptr<std::vector<std::optional<Bytes>>> log;
    loa::contracts::Awaitable<port::Result<>> write(std::vector<std::uint8_t> bytes) override {
        log->emplace_back(std::move(bytes));
        co_return port::Result<>{};
    }
    loa::contracts::Awaitable<port::Result<>> resize(GridSize) override { co_return port::Result<>{}; }
    loa::contracts::Awaitable<port::Result<>> close() override {
        log->emplace_back(std::nullopt);
        co_return port::Result<>{};
    }
};

} // namespace

TEST_CASE("remote channel streams both ways until the program ends") {
    auto log = std::make_shared<std::vector<std::optional<Bytes>>>();
    auto reader = std::make_unique<Script>();
    reader->reads = {port::channel_read::Data{b("\x1b[6n")}, port::channel_read::Closed{0}};
    auto writer = std::make_unique<Sent>();
    writer->log = log;
    auto host = std::make_shared<Host>();
    host->channel = PtyChannel{std::move(reader), std::move(writer)};
    auto s = state();
    s.with_remote(host);

    asio::io_context io;
    std::vector<Message> inbox;
    const Runner runner(io.get_executor(), [&](Message m) { inbox.push_back(std::move(m)); });
    auto [first, open] = s.open(remote("box"));
    auto [down, refused] = s.open(remote("down"));
    (void)s.update(msg::Canvas{first, canvas_event::KeyPress{Key::character("l"), {}, "l"}});
    runner.run(open, s.backend());
    runner.run(refused, s.backend());
    const auto poll = [&] {
        io.restart();
        return io.poll();
    };
    for (int i = 0; i < 1000 && (poll() > 0 || !inbox.empty()); ++i) {
        auto pending = std::exchange(inbox, {});
        for (auto& m : pending) runner.run(s.update(std::move(m)), s.backend());
    }
    const std::vector<std::optional<Bytes>> want{b("l"), b("\x1b[1;1R"), std::nullopt};
    CHECK(*log == want);
    CHECK(s.emulator(first) == nullptr);
    REQUIRE(s.status(down));
    CHECK(s.status(down)->kind == Status::Kind::Failed);
    CHECK(s.status(down)->error.find("refused") != std::string::npos);
}

TEST_CASE("grid and pointer geometry") {
    const Metrics m{14, 8, 16};
    const auto big = m.grid_for(803, 410), tiny = m.grid_for(3, 3);
    CHECK(big == GridSize{100, 25, 8, 16});
    CHECK(tiny == GridSize{2, 1, 8, 16});
    const GridSize grid{10, 5};
    CHECK(m.cell_at(17, 33, grid) == std::pair{ViewportCell{2, 2}, Side::Left});
    CHECK(m.cell_at(22, 33, grid) == std::pair{ViewportCell{2, 2}, Side::Right});
    CHECK(m.cell_at(-40, 900, grid) == std::pair{ViewportCell{0, 4}, Side::Left});
}

TEST_CASE("colours resolve from tokens, overrides and xterm tables") {
    namespace kt = loa::ui::kit::theme;
    const auto tokens = kt::builtin_tokens(kt::Variant::Light);
    const auto p = TermPalette::from_tokens(*tokens);
    Colors none{}, set{};
    set[COLOR_BACKGROUND] = Rgb{1, 2, 3};
    CHECK(p.resolve({CellColor::Kind::Background}, none) == tokens->color(kt::ColorRole::Ground));
    CHECK(p.resolve({CellColor::Kind::Indexed, 1}, none) == tokens->color(kt::ColorRole::Term1));
    CHECK(p.resolve({CellColor::Kind::Background}, set) == Color::rgb8(1, 2, 3));
    CHECK(p.resolve({CellColor::Kind::Indexed, 196}, none) == Color::rgb8(255, 0, 0));
    CHECK(p.resolve({CellColor::Kind::Indexed, 244}, none) == Color::rgb8(128, 128, 128));
}

TEST_CASE("the emulator tracks modes, colours and scrollback") {
    Emulator e(GridSize{10, 2});
    const auto feed = [&](std::string_view s) { e.advance(b(s)); };
    feed("\x1b[?1h\x1b[?2004h");
    CHECK(e.mode().contains(TermMode::APP_CURSOR | TermMode::BRACKETED_PASTE | TermMode::ALTERNATE_SCROLL));
    feed("\x1b[?1l\x1b[?1000h");
    CHECK_FALSE(e.mode().intersects(TermMode::APP_CURSOR));
    CHECK(e.mode().contains(TermMode::MOUSE_MODE));
    feed("\x1b]11;rgb:01/02/03\x07\x1b]4;1;#ff0000\x07");
    CHECK(e.colors()[COLOR_BACKGROUND] == Rgb{1, 2, 3});
    CHECK(e.colors()[1] == Rgb{255, 0, 0});
    feed("one\r\ntwo\r\nthree");
    CHECK(e.screen_text() == "two\nthree");
    e.scroll_display(Scroll::top());
    CHECK(e.display_offset() == 1);
    CHECK(e.screen_text() == "one\ntwo");
    e.scroll_display(Scroll::bottom());
    feed("\x1b[1;31mx");
    CHECK(e.cell({5, 1}).fg == CellColor{CellColor::Kind::Indexed, 1, {}});
    CHECK((e.cell({5, 1}).flags & flags::BOLD) != 0);
    e.select_start(SelectionType::Semantic, {1, 1}, Side::Left);
    CHECK(e.selection_text() == "threex");
    e.select_start(SelectionType::Lines, {0, 0}, Side::Left);
    e.select_update({0, 1}, Side::Left);
    CHECK(e.selection_text() == "two\nthreex");
}
