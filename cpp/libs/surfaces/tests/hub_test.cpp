// The hub with its real servers: documents from files, a VM over a fake RFB server, the surface
// tools as host tools and behind the loopback MCP endpoint, and declared surfaces of an MCP
// server opened as `McpSurface`s. Port of `loa-surfaces/tests/hub.rs`, the `tools.rs` unit test
// and the hub goldens (`surface/hub/*`).

#include <doctest/doctest.h>

#include <asio.hpp>
#include <asio/use_future.hpp>

#include <algorithm>
#include <chrono>
#include <fstream>
#include <future>
#include <sstream>

#include "loa/bus/proto.hpp"
#include "loa/contracts/surface_channel.hpp"
#include "loa/core/sink.hpp"
#include "loa/core/xml.hpp"
#include "loa/mcp/host_tools.hpp"
#include "loa/mcp/manager.hpp"
#include "loa/mcp/tool_filter.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/surfaces/hub.hpp"
#include "loa/surfaces/pixels.hpp"
#include "loa/surfaces/testkit.hpp"
#include "loa/surfaces/vnc/testkit.hpp"

using namespace loa::surfaces::hub;
namespace cs = loa::contracts::surface;
namespace json = loa::runtime::json;
namespace mcp = loa::mcp;
namespace rfb = loa::surfaces::testkit::rfb;
using loa::contracts::ImageBlock;
using loa::contracts::ToolOutput;
using loa::surfaces::testkit::next_frame;
using loa::surfaces::testkit::PATIENCE;

namespace {

std::string read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    REQUIRE(in);
    std::ostringstream text;
    text << in.rdbuf();
    return text.str();
}

std::string golden(const std::string& name) { return read_file(std::string(LOA_GOLDEN_DIR) + "/surface/hub/" + name); }

// Runs `body` on a four thread executor (Rust `#[tokio::test(flavor = "multi_thread")]`).
template <typename F>
void run(F body) {
    loa::runtime::Executor exec(4);
    auto done = asio::co_spawn(exec.context(), body(), asio::use_future);
    REQUIRE(done.wait_for(std::chrono::seconds(90)) == std::future_status::ready);
    done.get();
}

// A hub without a browser that writes `name` as a `width` x `height` PNG of one colour.
std::pair<std::shared_ptr<SurfaceHub>, std::filesystem::path> new_hub(const std::filesystem::path& dir, const std::string& name,
                                                                      std::uint32_t width, std::uint32_t height,
                                                                      loa::surfaces::Pixel color) {
    auto config = HubConfig::discover(dir);
    config.fonts = loa::surfaces::docs::Fonts::none();
    config.browser = std::nullopt;
#ifdef LOA_SURFACES_REMOTE_VIDEO
    // Software H.264 only: the same on every machine, and no ffmpeg probing.
    config.video.encoders = std::make_shared<const loa::surfaces::video::LiveVideo>(
        loa::surfaces::video::LiveVideo::with_probed(std::nullopt, loa::surfaces::video::probe(nullptr)));
#endif
    const auto image = dir / name;
    std::filesystem::create_directories(dir);
    const auto png = loa::surfaces::RgbaImage::filled(width, height, color).to_png();
    REQUIRE(png);
    std::ofstream(image, std::ios::binary).write(reinterpret_cast<const char*>(png->data()), static_cast<std::streamsize>(png->size()));
    return {SurfaceHub::make(std::move(config)), image};
}

std::optional<std::vector<std::uint8_t>> shot_png(const ToolOutput& output) {
    for (const auto& block : output.blocks) {
        if (const auto* image = std::get_if<ImageBlock>(&block)) {
            const auto* bytes = reinterpret_cast<const std::uint8_t*>(image->data.data());
            return std::vector<std::uint8_t>(bytes, bytes + image->data.size());
        }
    }
    return std::nullopt;
}

bool is_png(const std::vector<std::uint8_t>& bytes) {
    return bytes.size() >= 4 && bytes[0] == 0x89 && bytes[1] == 'P' && bytes[2] == 'N' && bytes[3] == 'G';
}

bool contains(std::string_view text, std::string_view part) { return text.find(part) != std::string_view::npos; }

asio::awaitable<void> sleep(std::chrono::milliseconds duration) {
    asio::steady_timer timer(co_await asio::this_coro::executor, duration);
    co_await timer.async_wait(asio::use_awaitable);
}

mcp::ServerSpec http_spec(const std::string& name, const std::string& url, const std::string& token) {
    auto http = mcp::HttpSpec::make(url, {{"Authorization", "Bearer " + token}});
    REQUIRE(http);
    auto spec = mcp::ServerSpec::make(name, std::move(*http));
    REQUIRE(spec);
    return std::move(*spec);
}

asio::awaitable<void> documents_and_vms_open_list_answer_tools_and_close() {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto [hub, image] = new_hub(dir->path(), "dot.png", 30, 20, {200, 10, 10, 255});
    auto events = hub->events();
    auto tools = SurfaceTools::make(hub);

    auto opened = co_await hub->open_document(image);
    REQUIRE(opened);
    CHECK(opened->origin == Origin::document());
    CHECK(opened->info.mode == cs::SurfaceMode::Document);
    CHECK(opened->info.width == 30);
    CHECK(opened->info.height == 20);
    auto event = co_await events.recv();
    const auto* was_opened = std::get_if<hub_event::Opened>(&event);
    REQUIRE(was_opened);
    CHECK(was_opened->origin == Origin::document());
    auto listed = co_await hub->list();
    REQUIRE(listed.size() == 1);
    CHECK(listed[0].open);
    const std::string uri = opened->info.uri.as_str();
    const json::Value no_args = json::Object{};
    auto list = co_await tools->run("surface_list", no_args, std::nullopt);
    CHECK_FALSE(list.is_error);
    CHECK(contains(list.joined_text(), uri));
    const json::Value uri_args = json::Object{{"uri", uri}};
    auto shot = co_await tools->run("surface_screenshot", uri_args, std::nullopt);
    const auto png_bytes = shot_png(shot);
    REQUIRE(png_bytes);
    CHECK(is_png(*png_bytes));
    const json::Value info_args = json::Object{{"uri", uri}, {"query", json::Object{{"kind", "info"}}}};
    auto info = co_await tools->run("surface_document_data", info_args, std::nullopt);
    CHECK(contains(info.joined_text(), "\"format\": \"png\""));
    const json::Value click_args = json::Object{{"uri", uri}, {"x", 1}, {"y", 1}};
    auto click = co_await tools->run("surface_click", click_args, std::nullopt);
    CHECK_MESSAGE(click.is_error, "documents take no clicks");

    // Host tools: text with a saved file, or the image itself.
    auto [specs, handler] = tools->host_tools();
    CHECK(std::ranges::any_of(specs, [](const auto& s) { return s.name == "surface_screenshot"; }));
    CHECK(std::ranges::all_of(specs, [](const auto& s) { return json::parse(s.input_schema).has_value(); }));
    const std::string args = json::dump(json::Object{{"uri", uri}});
    auto text = co_await handler->call("s1", "surface_screenshot", args);
    REQUIRE(text);
    const auto at = text->find(" saved to ");
    REQUIRE(at != std::string::npos);
    CHECK(text->starts_with("Screenshot (30x20)"));
    const auto saved = read_file(text->substr(at + std::string_view(" saved to ").size()));
    CHECK(std::vector<std::uint8_t>(saved.begin(), saved.end()) == *png_bytes);
    auto content = co_await handler->call_content("s1", "surface_screenshot", args);
    REQUIRE(content);
    REQUIRE(content->size() == 1);
    const auto* block = std::get_if<ImageBlock>(&content->front());
    REQUIRE(block);
    CHECK(block->mime == "image/png");
    CHECK(shot_png(ToolOutput{*content, false}) == png_bytes);
    auto missing = co_await handler->call("s1", "surface_screenshot", R"({"uri":"surface://docs/99"})");
    REQUIRE_FALSE(missing);
    CHECK(contains(missing.error(), "no surface"));
    REQUIRE(co_await hub->close(opened->info.uri));
    CHECK((co_await hub->list()).empty());
    event = co_await events.recv();
    CHECK(std::holds_alternative<hub_event::Closed>(event));

    // A VM from host and port streams and takes agent clicks.
    CHECK_FALSE(co_await hub->open_vm("no-port", std::nullopt));
    const std::array<std::uint8_t, 3> green{0, 200, 0};
    auto fake = co_await rfb::fake_rfb(8, 4, green);
    auto vm = co_await hub->open_vm(fake.address, std::nullopt);
    REQUIRE(vm);
    CHECK(vm->origin == Origin::vm());
    CHECK(vm->info.uri == *cs::SurfaceUri::make("vm", fake.address));
    auto frames = vm->surface->frames();
    REQUIRE(frames);
    fake.send_update();
    std::shared_ptr<const cs::Frame> frame;
    for (;;) {
        frame = co_await next_frame(**frames);
        if (!std::ranges::equal(std::span(frame->data).first(4), std::array<std::uint8_t, 4>{0, 0, 0, 255})) break;
    }
    CHECK(frame->width == 8);
    CHECK(frame->height == 4);
    CHECK(frame->encoding == cs::Encoding::Rgba);
    CHECK(std::ranges::equal(std::span(frame->data).first(4), std::array<std::uint8_t, 4>{0, 200, 0, 255}));
    const std::string vm_uri = vm->info.uri.as_str();
    const json::Value vm_click_args = json::Object{{"uri", vm_uri}, {"x", 3}, {"y", 2}};
    auto vm_click = co_await tools->run("surface_click", vm_click_args, "vm");
    CHECK_MESSAGE(!vm_click.is_error, vm_click.joined_text());
    const std::array<std::uint8_t, 6> pressed_at{5, 1, 0, 3, 0, 2};
    const auto deadline = std::chrono::steady_clock::now() + PATIENCE;
    for (;;) {
        {
            std::lock_guard lock(fake.received->mutex);
            if (std::ranges::search(fake.received->bytes, pressed_at).begin() != fake.received->bytes.end()) break;
        }
        REQUIRE_MESSAGE(std::chrono::steady_clock::now() < deadline, "the click reached the RFB server as a pointer event");
        co_await sleep(std::chrono::milliseconds(10));
    }
    const json::Value vm_args = json::Object{{"uri", vm_uri}};
    CHECK((co_await tools->run("surface_screenshot", vm_args, "browser")).is_error);
    REQUIRE(co_await hub->close(vm->info.uri));
    // The endpoint stays listed for reconnecting.
    listed = co_await hub->list();
    REQUIRE(listed.size() == 1);
    CHECK_FALSE(listed[0].open);
    CHECK(listed[0].origin == Origin::vm());
    co_await hub->shutdown();
}

asio::awaitable<void> the_mcp_endpoint_declares_surfaces_and_a_second_hub_opens_them() {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto [hub, image] = new_hub(dir->path(), "blue.png", 12, 6, {0, 0, 255, 255});
    auto doc = co_await hub->open_document(image);
    REQUIRE(doc);
    const std::array<std::uint8_t, 3> yellow{250, 250, 0};
    auto fake = co_await rfb::fake_rfb(6, 3, yellow);
    auto vm = co_await hub->open_vm(fake.address, std::nullopt);
    REQUIRE(vm);
    fake.send_update();

    auto endpoint = co_await McpEndpoint::start(SurfaceTools::make(hub));
    REQUIRE(endpoint);
    std::vector<std::string> names;
    for (const auto& spec : endpoint->specs()) names.push_back(spec.name);
    CHECK(names == std::vector<std::string>{"surface-browser", "surface-docs", "surface-vm"});
    auto manager = mcp::Manager::make();
    REQUIRE(manager);
    for (const std::string server : {"docs", "vm"}) {
        const std::string name = "surface-" + server;
        REQUIRE(manager->add(http_spec(name, endpoint->url(server), endpoint->token())));
        auto status = manager->watch(name);
        REQUIRE(status);
        auto running = status->wait_for(
            [&](const mcp::Status& s) {
                if (s.kind == mcp::Status::Kind::Error) MESSAGE(name << ": " << s.message);
                return s.kind == mcp::Status::Kind::Running;
            },
            std::chrono::duration_cast<std::chrono::milliseconds>(PATIENCE));
        REQUIRE_MESSAGE(running, "the MCP server runs");
    }

    auto docs_tools = manager->tools("surface-docs", mcp::AllowAll{});
    REQUIRE(docs_tools);
    const auto has_tool = [&](std::string_view name) {
        return std::ranges::any_of(*docs_tools, [&](const auto& t) { return t.name == name; });
    };
    CHECK(has_tool("surface_document_data"));
    CHECK_FALSE(has_tool("surface_click"));
    auto declared = manager->surfaces("surface-docs");
    REQUIRE(declared);
    REQUIRE(declared->size() == 1);
    CHECK(declared->front().first == doc->info.uri);
    auto called = manager->call_tool("surface-vm", "surface_screenshot", json::Object{{"uri", vm->info.uri.as_str()}}, mcp::AllowAll{});
    REQUIRE(called);
    const auto vm_shot = shot_png(mcp::host_tools::to_output(*called));
    CHECK((vm_shot && is_png(*vm_shot)));
    // Wrong token: refused.
    auto intruder = mcp::Manager::make();
    REQUIRE(intruder);
    CHECK_MESSAGE(!intruder->probe(http_spec("x", endpoint->url("vm"), "nope")), "a bad token is refused");

    // A second hub sees them as third party MCP surfaces and opens them.
    auto [other, unused] = new_hub(dir->path() / "other", "unused.png", 1, 1, {0, 0, 0, 0});
    CHECK((co_await other->refresh_declared(*manager)).empty());
    auto listed = co_await other->list();
    REQUIRE(listed.size() == 2);
    CHECK(std::ranges::all_of(listed, [](const Listed& l) { return !l.open && l.origin.kind() == Origin::Kind::Mcp; }));

    auto remote_vm = co_await other->open(vm->info.uri);
    REQUIRE(remote_vm);
    CHECK(remote_vm->origin == Origin::mcp("surface-vm"));
    auto frames = remote_vm->surface->frames();
    REQUIRE(frames);
    auto frame = co_await next_frame(**frames);
    CHECK(frame->encoding == cs::Encoding::Png);
    CHECK(frame->width == 6);
    CHECK(frame->height == 3);
    // A click through the MCP surface reaches the RFB server and stamps the next frame.
    for (const bool pressed : {true, false}) {
        cs::InputEvent event{7, 0, cs::input::MouseButton{1.0F, 1.0F, cs::MouseButton::Left, pressed, {}}};
        REQUIRE(co_await remote_vm->surface->input(std::move(event)));
    }
    for (;;) {
        frame = co_await next_frame(**frames);
        if (frame->input_seq == 7) break;
    }
    CHECK(frame->keyframe);

    // Document mode: the file comes over once through resources/read.
    auto remote_doc = co_await other->open(doc->info.uri);
    REQUIRE(remote_doc);
    auto document = co_await remote_doc->surface->document();
    REQUIRE(document);
    CHECK(document->mime == "image/png");
    const auto file = read_file(image);
    CHECK(document->data == std::vector<std::uint8_t>(file.begin(), file.end()));
    auto doc_shot = co_await remote_doc->surface->screenshot();
    REQUIRE(doc_shot);
    CHECK(is_png(*doc_shot));
    const json::Value doc_args = json::Object{{"uri", doc->info.uri.as_str()}};
    auto shot = co_await SurfaceTools::make(other)->run("surface_screenshot", doc_args, std::nullopt);
    const auto other_shot = shot_png(shot);
    CHECK((other_shot && is_png(*other_shot)));
    manager->shutdown();
    co_await other->shutdown();
    co_await hub->shutdown();
}

asio::awaitable<void> input_sequence_is_shared_by_host_scoped_and_mcp_tool_sets() {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto config = HubConfig::discover(dir->path());
    config.browser = std::nullopt;
    auto hub = SurfaceHub::make(std::move(config));
    const std::array<std::uint8_t, 3> black{0, 0, 0};
    auto fake = co_await rfb::fake_rfb(8, 4, black);
    auto opened = co_await hub->open_vm(fake.address, std::nullopt);
    REQUIRE(opened);
    fake.send_update();
    auto tools = SurfaceTools::make(hub);
    const json::Value args = json::Object{{"uri", opened->info.uri.as_str()}, {"x", 1}, {"y", 1}, {"dx", 0}, {"dy", 1}};

    auto [_, host] = tools->host_tools();
    REQUIRE(co_await host->call("session", std::string(SCROLL), json::dump(args)));
    CHECK(hub->tool_input_seq() == 1);
    for (std::uint64_t expected = 2; expected <= 3; ++expected) {
        CHECK_FALSE((co_await tools->run(std::string(SCROLL), args, "vm")).is_error);
        CHECK(hub->tool_input_seq() == expected);
    }
    const auto mcp_set = tool_set(hub, "vm");
    const loa::contracts::CallCtx call;
    auto result = co_await mcp_set.call(call, std::string(SCROLL), args);
    REQUIRE(result);
    CHECK_FALSE(result->is_error);
    CHECK(hub->tool_input_seq() == 4);
    auto other = SurfaceTools::make(hub);
    CHECK_FALSE((co_await other->run(std::string(SCROLL), args, std::nullopt)).is_error);
    CHECK(hub->tool_input_seq() == 5);
    CHECK((co_await tools->run(std::string(SCROLL), args, "browser")).is_error);
    CHECK(hub->tool_input_seq() == 5);
    co_await hub->shutdown();
}

// Appends `bytes` with a varint length in front (prost `encode_length_delimited`).
void length_delimited(std::vector<std::uint8_t>& out, const std::vector<std::uint8_t>& bytes) {
    std::uint64_t n = bytes.size();
    while (n >= 0x80) {
        out.push_back(static_cast<std::uint8_t>(n | 0x80));
        n >>= 7;
    }
    out.push_back(static_cast<std::uint8_t>(n));
    out.insert(out.end(), bytes.begin(), bytes.end());
}

} // namespace

TEST_CASE("hub: documents and VMs open, list, answer tools and close") { run(documents_and_vms_open_list_answer_tools_and_close); }

TEST_CASE("hub: the MCP endpoint declares surfaces and a second hub opens them") {
    run(the_mcp_endpoint_declares_surfaces_and_a_second_hub_opens_them);
}

TEST_CASE("hub: the input sequence is shared by host, scoped and MCP tool sets") {
    run(input_sequence_is_shared_by_host_scoped_and_mcp_tool_sets);
}

#if !defined(__linux__)
TEST_CASE("hub: the virtual display says it needs Linux elsewhere") {
    auto error = VirtualDisplaySetup::find(std::nullopt, std::nullopt);
    REQUIRE_FALSE(error);
    CHECK(error.error() == "needs Linux (Xvfb)");
}
#endif

TEST_CASE("hub golden: surface tool names, schemas and tool_set lists match the Rust output") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto config = HubConfig::discover(dir->path());
    config.browser = std::nullopt;
    auto hub = SurfaceHub::make(std::move(config));
    auto expected = json::parse(golden("tools.json"));
    REQUIRE(expected);
    const std::pair<std::string_view, std::optional<std::string_view>> scopes[] = {
        {"all", std::nullopt}, {"browser", "browser"}, {"docs", "docs"}, {"vm", "vm"}};
    for (const auto& [key, scope] : scopes) {
        CAPTURE(key);
        const auto* want = expected->find(key);
        REQUIRE(want);
        REQUIRE(want->as_array());
        const auto specs = tool_set(hub, scope).specs();
        REQUIRE(specs.size() == want->as_array()->size());
        for (std::size_t i = 0; i < specs.size(); ++i) {
            const auto& w = (*want->as_array())[i];
            CAPTURE(specs[i].name);
            CHECK(specs[i].name == *w.find("name")->as_string());
            CHECK(specs[i].description == *w.find("description")->as_string());
            CHECK(specs[i].input_schema == *w.find("input_schema")->as_string());
        }
    }
}

TEST_CASE("hub golden: <surface> elements serialize to the Rust DOM and patches") {
    loa::core::MemorySink sink;
    const loa::core::Attrs session_attrs{{"id", loa::core::Value("s1")},
                                         {"harness", loa::core::Value("omp")},
                                         {"node", loa::core::Value("key-laptop")},
                                         {"created", loa::core::Value(std::int64_t{100})}};
    auto root = sink.commit_at(loa::core::Draft::insert(loa::core::NodeId::DOCUMENT, loa::core::ElementKind::Session, session_attrs), 1);
    REQUIRE(root);
    const cs::SurfaceInfo tab{*cs::SurfaceUri::make("browser", "tab1"), "Example", 1280, 720, cs::SurfaceMode::Pixel, std::nullopt};
    const cs::SurfaceInfo doc{*cs::SurfaceUri::make("files", "report"), "report.pdf", 0, 0, cs::SurfaceMode::Document, "application/pdf"};
    auto first = sink.commit_at(dom::opened(root->node, tab, Origin::browser()), 2);
    REQUIRE(first);
    REQUIRE(sink.commit_at(dom::opened(root->node, doc, Origin::mcp("files-server")), 3));
    REQUIRE(sink.commit_at(dom::closed(first->node), 4));

    std::vector<std::uint8_t> encoded;
    for (const auto& patch : sink.patches()) length_delimited(encoded, loa::bus::proto::encode_patch(patch));
    const auto want = golden("dom-patches.bin");
    CHECK(encoded == std::vector<std::uint8_t>(want.begin(), want.end()));
    CHECK(loa::core::xml::to_string(sink.dom()) == golden("dom.xml"));

    // The golden patches decode and re-encode byte for byte.
    std::vector<std::uint8_t> again;
    std::span<const std::uint8_t> rest(reinterpret_cast<const std::uint8_t*>(want.data()), want.size());
    while (!rest.empty()) {
        std::uint64_t n = 0;
        std::size_t used = 0;
        for (int shift = 0;; shift += 7) {
            const auto b = rest[used++];
            n |= std::uint64_t(b & 0x7f) << shift;
            if (!(b & 0x80)) break;
        }
        auto patch = loa::bus::proto::decode_patch(rest.subspan(used, n));
        REQUIRE(patch);
        length_delimited(again, loa::bus::proto::encode_patch(*patch));
        rest = rest.subspan(used + n);
    }
    CHECK(again == std::vector<std::uint8_t>(want.begin(), want.end()));
}

TEST_CASE("hub: surface resources round-trip through MCP metadata") {
    const cs::SurfaceInfo info{*cs::SurfaceUri::make("docs", "1"), "a.pdf", 10, 20, cs::SurfaceMode::Document, "application/pdf"};
    auto parsed = parse_resource(resource(info));
    REQUIRE(parsed);
    CHECK(*parsed == info);
    CHECK(*parse_resource(json::Object{{"uri", "https://x"}, {"name", "n"}}) == std::nullopt);
    CHECK(parse_resource(json::Object{{"uri", "surface://a/b"}, {"name", "n"}}).error() ==
          "surface resource surface://a/b lacks `_meta.\"omp/surface\"` with width, height and mode");
}

TEST_CASE("hub: addresses typed without a scheme get one") {
    CHECK(normalize_url("  example.com ") == "https://example.com");
    CHECK(normalize_url("localhost:3000") == "http://localhost:3000");
    CHECK(normalize_url("127.0.0.1") == "http://127.0.0.1");
    CHECK(normalize_url("about:blank") == "about:blank");
    CHECK(normalize_url("ftp://x") == "ftp://x");
    CHECK(normalize_url("") == "");
}

TEST_CASE("hub: errors read like the Rust crate") {
    CHECK(HubError::address("x").message() == "`x` isn't a host:port address");
    CHECK(HubError::not_found(*cs::SurfaceUri::make("docs", "9")).message() == "no surface surface://docs/9");
    CHECK(HubError::no_mcp().message() == "the MCP servers aren't connected");
    CHECK(HubError::no_virtual_display("needs Linux (Xvfb)").message() == "the virtual display can't run here: needs Linux (Xvfb)");
    CHECK(HubError::not_found(*cs::SurfaceUri::make("docs", "9")).to_surface().kind() == cs::SurfaceError::Kind::NotFound);
    CHECK(debug(open_request::Vm{"a:1", std::nullopt}) == "Vm { address: \"a:1\", password: None }");
}
