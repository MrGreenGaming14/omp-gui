#include "loa/surfaces/hub/tools.hpp"

#include <asio.hpp>

#include <unistd.h>

#include <cerrno>
#include <charconv>
#include <cstdio>
#include <cstring>
#include <limits>
#include <system_error>

#include "loa/contracts/schema.hpp"
#include "loa/runtime/time.hpp"
#include "loa/surfaces/browser/server.hpp"
#include "loa/surfaces/docs/server.hpp"
#include "loa/surfaces/hub/service.hpp"
#include "loa/surfaces/pixels.hpp"
#include "loa/surfaces/vnc/vnc.hpp"

namespace loa::surfaces::hub {

namespace json = loa::runtime::json;
namespace s = loa::contracts::schema;
namespace args = loa::contracts::args;
using contracts::Awaitable;
using contracts::CallCtx;
using contracts::ImageBlock;
using contracts::Json;
using contracts::OutputBlock;
using contracts::TextBlock;
using contracts::Tool;
using contracts::ToolOutput;
using contracts::ToolResult;
using contracts::ToolSet;

namespace {

constexpr std::string_view URI_DOC = "The surface, e.g. surface://browser/tab1";

// ---- argument decoding (serde style errors) -----------------------------------

template <typename T>
using Parsed = std::expected<T, std::string>;

std::string backticked(std::string_view s) { return "`" + std::string(s) + "`"; }

// serde's "expected one of" list.
std::string expected_list(std::initializer_list<std::string_view> names) {
    if (names.size() == 0) return "there are no fields";
    std::vector<std::string_view> all(names);
    if (all.size() == 1) return "expected " + backticked(all[0]);
    if (all.size() == 2) return "expected " + backticked(all[0]) + " or " + backticked(all[1]);
    std::string out = "expected one of ";
    for (std::size_t i = 0; i < all.size(); ++i) out += (i ? ", " : "") + backticked(all[i]);
    return out;
}

// An object of only `fields` (`#[serde(deny_unknown_fields)]`).
Parsed<void> strict(const Json& json, std::string_view what, std::initializer_list<std::string_view> fields) {
    if (auto ok = args::object(json, what); !ok) return std::unexpected(ok.error());
    for (const auto& [key, _] : *json.as_object()) {
        if (std::ranges::find(fields, key) == fields.end()) {
            const auto list = expected_list(fields);
            return std::unexpected("unknown field " + backticked(key) + ", " + list);
        }
    }
    return {};
}

Parsed<float> f32_field(const Json& json, std::string_view name, std::optional<float> fallback = std::nullopt) {
    auto value = args::optional_f64(json, name);
    if (!value) return std::unexpected(value.error());
    if (*value) return static_cast<float>(**value);
    if (fallback) return *fallback;
    return std::unexpected("missing field " + backticked(name));
}

template <typename E>
Parsed<E> variant_of(std::string_view text, std::initializer_list<std::pair<std::string_view, E>> variants) {
    for (const auto& [name, value] : variants) {
        if (name == text) return value;
    }
    std::string list = "expected one of ";
    bool first = true;
    for (const auto& [name, _] : variants) {
        list += (first ? "" : ", ") + backticked(name);
        first = false;
    }
    return std::unexpected("unknown variant " + backticked(text) + ", " + list);
}

// ---- argument types --------------------------------------------------------

struct NoArgs {
    static std::string schema() { return s::Root("NoArgs", s::Object().deny_unknown_fields()).dump(); }
    static Parsed<NoArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct NoArgs", {}); !ok) return std::unexpected(ok.error());
        return NoArgs{};
    }
};

struct UriArgs {
    std::string uri;
    static std::string schema() {
        return s::Root("UriArgs", s::Object().property("uri", s::describe(s::string(), URI_DOC)).deny_unknown_fields()).dump();
    }
    static Parsed<UriArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct UriArgs", {"uri"}); !ok) return std::unexpected(ok.error());
        auto uri = args::string(json, "uri");
        if (!uri) return std::unexpected(uri.error());
        return UriArgs{std::move(*uri)};
    }
};

enum class ModifierKey : std::uint8_t { Shift, Ctrl, Alt, Meta };

struct ClickArgs {
    std::string uri;
    float x = 0, y = 0;
    cs::MouseButton button = cs::MouseButton::Left;
    std::uint32_t count = 1;
    std::vector<ModifierKey> modifiers;

    static std::string schema() {
        auto object = s::Object()
                          .property("uri", s::describe(s::string(), URI_DOC))
                          .property("x", s::describe(s::number("float"), "Pixels from the left edge of the surface"))
                          .property("y", s::describe(s::number("float"), "Pixels from the top edge of the surface"))
                          .property("button", s::ref("ClickButton"), false)
                          .property("count",
                                    s::describe(s::with_default(s::range(s::unsigned_integer("uint32"), 1, 3), 1),
                                                "2 for a double click, 3 for a triple click"),
                                    false)
                          .property("modifiers", s::describe(s::array(s::ref("ModifierKey")), "Keys held during the click"), false)
                          .deny_unknown_fields();
        return s::Root("ClickArgs", std::move(object))
            .define("ClickButton", s::string_enum({"left", "middle", "right"}))
            .define("ModifierKey", s::string_enum({"shift", "ctrl", "alt", "meta"}))
            .dump();
    }

    static Parsed<ClickArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct ClickArgs", {"uri", "x", "y", "button", "count", "modifiers"}); !ok) {
            return std::unexpected(ok.error());
        }
        ClickArgs out;
        auto uri = args::string(json, "uri");
        if (!uri) return std::unexpected(uri.error());
        out.uri = std::move(*uri);
        auto x = f32_field(json, "x");
        if (!x) return std::unexpected(x.error());
        auto y = f32_field(json, "y");
        if (!y) return std::unexpected(y.error());
        out.x = *x;
        out.y = *y;
        auto button = args::optional_string(json, "button");
        if (!button) return std::unexpected(button.error());
        if (*button) {
            auto parsed = variant_of<cs::MouseButton>(
                **button, {{"left", cs::MouseButton::Left}, {"middle", cs::MouseButton::Middle}, {"right", cs::MouseButton::Right}});
            if (!parsed) return std::unexpected(parsed.error());
            out.button = *parsed;
        }
        auto count = args::optional_u64(json, "count");
        if (!count) return std::unexpected(count.error());
        if (*count) {
            if (**count > std::numeric_limits<std::uint32_t>::max()) {
                return std::unexpected("invalid value: integer `" + std::to_string(**count) + "`, expected u32");
            }
            out.count = static_cast<std::uint32_t>(**count);
        }
        if (const auto* modifiers = json.find("modifiers"); modifiers && !modifiers->is_null()) {
            const auto* list = modifiers->as_array();
            if (!list) return std::unexpected(std::string("invalid type: expected a sequence"));
            for (const auto& item : *list) {
                const auto* name = item.as_string();
                if (!name) return std::unexpected(std::string("invalid type: expected variant identifier"));
                auto key = variant_of<ModifierKey>(
                    *name, {{"shift", ModifierKey::Shift}, {"ctrl", ModifierKey::Ctrl}, {"alt", ModifierKey::Alt}, {"meta", ModifierKey::Meta}});
                if (!key) return std::unexpected(key.error());
                out.modifiers.push_back(*key);
            }
        }
        return out;
    }
};

struct TypeArgs {
    std::string uri;
    std::string text;
    bool submit = false;

    static std::string schema() {
        auto object = s::Object()
                          .property("uri", s::describe(s::string(), URI_DOC))
                          .property("text", s::string())
                          .property("submit", s::describe(s::with_default(s::boolean(), false), "Press Enter after the text"), false)
                          .deny_unknown_fields();
        return s::Root("TypeArgs", std::move(object)).dump();
    }
    static Parsed<TypeArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct TypeArgs", {"uri", "text", "submit"}); !ok) return std::unexpected(ok.error());
        auto uri = args::string(json, "uri");
        if (!uri) return std::unexpected(uri.error());
        auto text = args::string(json, "text");
        if (!text) return std::unexpected(text.error());
        auto submit = args::optional_bool(json, "submit");
        if (!submit) return std::unexpected(submit.error());
        return TypeArgs{std::move(*uri), std::move(*text), submit->value_or(false)};
    }
};

struct ScrollArgs {
    std::string uri;
    float x = 0, y = 0, dx = 0, dy = 0;

    static std::string schema() {
        auto object = s::Object()
                          .property("uri", s::describe(s::string(), URI_DOC))
                          .property("x", s::number("float"))
                          .property("y", s::number("float"))
                          .property("dx", s::describe(s::with_default(s::number("float"), 0.0), "Pixels to scroll right"), false)
                          .property("dy", s::describe(s::with_default(s::number("float"), 0.0), "Pixels to scroll down"), false)
                          .deny_unknown_fields();
        return s::Root("ScrollArgs", std::move(object)).dump();
    }
    static Parsed<ScrollArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct ScrollArgs", {"uri", "x", "y", "dx", "dy"}); !ok) return std::unexpected(ok.error());
        auto uri = args::string(json, "uri");
        if (!uri) return std::unexpected(uri.error());
        auto x = f32_field(json, "x");
        if (!x) return std::unexpected(x.error());
        auto y = f32_field(json, "y");
        if (!y) return std::unexpected(y.error());
        auto dx = f32_field(json, "dx", 0.0F);
        if (!dx) return std::unexpected(dx.error());
        auto dy = f32_field(json, "dy", 0.0F);
        if (!dy) return std::unexpected(dy.error());
        return ScrollArgs{std::move(*uri), *x, *y, *dx, *dy};
    }
};

struct UrlArgs {
    std::string url;
    static std::string schema() { return s::Root("UrlArgs", s::Object().property("url", s::string()).deny_unknown_fields()).dump(); }
    static Parsed<UrlArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct UrlArgs", {"url"}); !ok) return std::unexpected(ok.error());
        auto url = args::string(json, "url");
        if (!url) return std::unexpected(url.error());
        return UrlArgs{std::move(*url)};
    }
};

struct NavigateArgs {
    std::string uri;
    std::string url;
    static std::string schema() {
        auto object = s::Object().property("uri", s::describe(s::string(), URI_DOC)).property("url", s::string()).deny_unknown_fields();
        return s::Root("NavigateArgs", std::move(object)).dump();
    }
    static Parsed<NavigateArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct NavigateArgs", {"uri", "url"}); !ok) return std::unexpected(ok.error());
        auto uri = args::string(json, "uri");
        if (!uri) return std::unexpected(uri.error());
        auto url = args::string(json, "url");
        if (!url) return std::unexpected(url.error());
        return NavigateArgs{std::move(*uri), std::move(*url)};
    }
};

struct DataArgs {
    std::string uri;
    docs::DataQuery query;

    static std::string schema() {
        auto variant = [](std::string_view kind, std::string_view description) {
            return s::Object().description(std::string(description)).property("kind", s::constant(kind));
        };
        auto text = variant("text", "Page text (PDF, one entry per page or just `page`), document text (DOCX) or slide text (PPTX).")
                        .property("page", s::with_default(s::nullable(s::unsigned_integer("uint")), nullptr), false);
        auto cells = variant("cells", "XLSX: cells of `range` (A1 notation) in `sheet` (default: the first).")
                         .property("sheet", s::with_default(s::nullable(s::string()), nullptr), false)
                         .property("range", s::string());
        auto query = s::one_of({variant("info", "Format, MIME type, page count and page sizes. Every format.").to_json(), text.to_json(),
                                variant("sheets", "XLSX: sheet names and used sizes.").to_json(), cells.to_json(),
                                variant("outline", "DOCX: paragraphs with style, heading level, list level and runs, and tables.").to_json(),
                                variant("slides", "PPTX: slides in presentation order with their shapes, text and notes.").to_json()},
                               "Structured data the agent tools ask a document surface for.");
        auto object = s::Object()
                          .property("uri", s::describe(s::string(), "The surface, e.g. surface://docs/1"))
                          .property("query", s::ref("DataQuery"))
                          .deny_unknown_fields();
        return s::Root("DataArgs", std::move(object)).define("DataQuery", std::move(query)).dump();
    }
    static Parsed<DataArgs> from_json(const Json& json) {
        if (auto ok = strict(json, "struct DataArgs", {"uri", "query"}); !ok) return std::unexpected(ok.error());
        auto uri = args::string(json, "uri");
        if (!uri) return std::unexpected(uri.error());
        const auto* query = json.find("query");
        if (!query) return std::unexpected(std::string("missing field `query`"));
        auto parsed = docs::DataQuery::from_json(*query);
        if (!parsed) return std::unexpected(parsed.error().message());
        return DataArgs{std::move(*uri), std::move(*parsed)};
    }
};

// ---- helpers ---------------------------------------------------------------

// A float as Rust's `Display` writes it: shortest round-trip digits, never an exponent.
std::string fmt(float v) {
    char buf[64];
    const auto res = std::to_chars(buf, buf + sizeof buf, v, std::chars_format::fixed);
    return std::string(buf, res.ptr);
}

std::size_t char_count(std::string_view s) {
    return static_cast<std::size_t>(std::ranges::count_if(s, [](char c) { return (static_cast<unsigned char>(c) & 0xC0) != 0x80; }));
}

ToolResult pretty(const json::Value& value) { return ToolOutput::text(json::dump_pretty(value)); }

// What every tool works on: the hub and the built-in server it is limited to.
struct Ctx {
    std::shared_ptr<SurfaceHub> hub;
    std::optional<std::string> scope;

    Parsed<cs::SurfaceUri> uri(std::string_view text) const {
        auto uri = cs::SurfaceUri::parse(text);
        if (!uri) return std::unexpected(uri.error().message());
        if (scope && uri->server() != *scope) return std::unexpected(uri->as_str() + " isn't a " + *scope + " surface");
        return std::move(*uri);
    }

    Awaitable<Parsed<Opened>> opened(std::string text) const {
        auto parsed = uri(text);
        if (!parsed) co_return std::unexpected(std::move(parsed.error()));
        auto opened = co_await hub->open(std::move(*parsed));
        if (!opened) co_return std::unexpected(opened.error().message());
        co_return std::move(*opened);
    }

    // A surface that takes input; documents are refused.
    Awaitable<Parsed<Opened>> pixel(std::string text) const {
        auto opened = co_await this->opened(std::move(text));
        if (!opened) co_return opened;
        if (opened->info.mode == cs::SurfaceMode::Document) {
            co_return std::unexpected(opened->info.uri.as_str() + " is a document; read it with " + std::string(DOCUMENT_DATA) + " or " +
                                      std::string(SCREENSHOT));
        }
        co_return opened;
    }

    Awaitable<Parsed<std::shared_ptr<browser::Tab>>> tab(std::string text) const {
        auto parsed = uri(text);
        if (!parsed) co_return std::unexpected(std::move(parsed.error()));
        auto tab = co_await hub->tab(std::move(*parsed));
        if (!tab) co_return std::unexpected(tab.error().message());
        co_return std::move(*tab);
    }

    Awaitable<Parsed<void>> input(const Opened& opened, std::vector<cs::Input> inputs) const {
        for (auto& input : inputs) {
            const auto seq = hub->next_tool_input_seq();
            auto sent = co_await opened.surface->input(cs::InputEvent{seq, runtime::time::now_us(), std::move(input)});
            if (!sent) co_return std::unexpected(sent.error().message());
        }
        co_return Parsed<void>{};
    }
};

// A tool on `Ctx`.
template <typename A>
class CtxTool : public Tool<A> {
  public:
    CtxTool(Ctx ctx, std::string_view name, std::string_view description) : c(std::move(ctx)), name_(name), description_(description) {}
    std::string_view name() const final { return name_; }
    std::string_view description() const final { return description_; }

  protected:
    Ctx c;

  private:
    std::string_view name_;
    std::string_view description_;
};

class List final : public CtxTool<NoArgs> {
  public:
    explicit List(Ctx ctx)
        : CtxTool(std::move(ctx), LIST, "List the surfaces open in the app (browser tabs, documents, VMs) with their uri, size and mode.") {}
    Awaitable<ToolResult> run(const CallCtx&, NoArgs) const override {
        const auto listed = co_await c.hub->list();
        json::Array rows;
        for (const auto& l : listed) {
            if (c.scope && l.info.uri.server() != *c.scope) continue;
            const auto& i = l.info;
            rows.emplace_back(json::Object{{"uri", i.uri.as_str()},
                                           {"name", i.name},
                                           {"width", std::uint64_t{i.width}},
                                           {"height", std::uint64_t{i.height}},
                                           {"mode", std::string(core::as_str(i.mode))},
                                           {"mime", i.mime ? json::Value(*i.mime) : json::Value(nullptr)},
                                           {"server", std::string(l.origin.server())},
                                           {"open", l.open}});
        }
        co_return pretty(std::move(rows));
    }
};

class Screenshot final : public CtxTool<UriArgs> {
  public:
    explicit Screenshot(Ctx ctx) : CtxTool(std::move(ctx), SCREENSHOT, "Capture the surface as a PNG image.") {}
    Awaitable<ToolResult> run(const CallCtx&, UriArgs a) const override {
        auto opened = co_await c.opened(std::move(a.uri));
        if (!opened) co_return std::unexpected(std::move(opened.error()));
        auto png = co_await opened->surface->screenshot();
        if (!png) co_return std::unexpected(png.error().message());
        const auto* bytes = reinterpret_cast<const std::byte*>(png->data());
        co_return ToolOutput{{ImageBlock{"image/png", std::vector<std::byte>(bytes, bytes + png->size())}}, false};
    }
};

class Click final : public CtxTool<ClickArgs> {
  public:
    explicit Click(Ctx ctx) : CtxTool(std::move(ctx), CLICK, "Click at a point of the surface, in surface pixels.") {}
    Awaitable<ToolResult> run(const CallCtx&, ClickArgs a) const override {
        if (a.count < 1 || a.count > 3) co_return std::unexpected("bad arguments for " + std::string(CLICK) + ": count must be 1, 2 or 3");
        auto opened = co_await c.pixel(a.uri);
        if (!opened) co_return std::unexpected(std::move(opened.error()));
        const auto has = [&](ModifierKey k) { return std::ranges::find(a.modifiers, k) != a.modifiers.end(); };
        const cs::Modifiers modifiers{has(ModifierKey::Shift), has(ModifierKey::Ctrl), has(ModifierKey::Alt), has(ModifierKey::Meta)};
        std::vector<cs::Input> inputs{cs::input::MouseMove{a.x, a.y}};
        for (std::uint32_t i = 0; i < a.count; ++i) {
            for (const bool pressed : {true, false}) inputs.emplace_back(cs::input::MouseButton{a.x, a.y, a.button, pressed, modifiers});
        }
        if (auto sent = co_await c.input(*opened, std::move(inputs)); !sent) co_return std::unexpected(std::move(sent.error()));
        co_return ToolOutput::text("Clicked (" + fmt(a.x) + ", " + fmt(a.y) + ") on " + opened->info.uri.as_str());
    }
};

class Type final : public CtxTool<TypeArgs> {
  public:
    explicit Type(Ctx ctx) : CtxTool(std::move(ctx), TYPE, "Type text into the focused element of the surface.") {}
    Awaitable<ToolResult> run(const CallCtx&, TypeArgs a) const override {
        auto opened = co_await c.pixel(a.uri);
        if (!opened) co_return std::unexpected(std::move(opened.error()));
        std::vector<cs::Input> inputs{cs::input::Text{a.text}};
        if (a.submit) {
            for (const bool pressed : {true, false}) inputs.emplace_back(cs::input::Key{"Enter", "Enter", pressed, {}});
        }
        if (auto sent = co_await c.input(*opened, std::move(inputs)); !sent) co_return std::unexpected(std::move(sent.error()));
        co_return ToolOutput::text("Typed " + std::to_string(char_count(a.text)) + " characters on " + opened->info.uri.as_str());
    }
};

class Scroll final : public CtxTool<ScrollArgs> {
  public:
    explicit Scroll(Ctx ctx)
        : CtxTool(std::move(ctx), SCROLL, "Scroll at a point of the surface. Positive dy scrolls the content down.") {}
    Awaitable<ToolResult> run(const CallCtx&, ScrollArgs a) const override {
        auto opened = co_await c.pixel(a.uri);
        if (!opened) co_return std::unexpected(std::move(opened.error()));
        std::vector<cs::Input> inputs{cs::input::Scroll{a.x, a.y, a.dx, a.dy, {}}};
        if (auto sent = co_await c.input(*opened, std::move(inputs)); !sent) co_return std::unexpected(std::move(sent.error()));
        co_return ToolOutput::text("Scrolled (" + fmt(a.dx) + ", " + fmt(a.dy) + ") at (" + fmt(a.x) + ", " + fmt(a.y) + ") on " +
                                   opened->info.uri.as_str());
    }
};

class OpenBrowser final : public CtxTool<UrlArgs> {
  public:
    explicit OpenBrowser(Ctx ctx)
        : CtxTool(std::move(ctx), OPEN_BROWSER,
                  "Open a new browser tab at a URL. The user sees it in the Surfaces panel. Returns its surface uri.") {}
    Awaitable<ToolResult> run(const CallCtx&, UrlArgs a) const override {
        auto opened = co_await c.hub->open_browser(std::move(a.url));
        if (!opened) co_return std::unexpected(opened.error().message());
        const auto title = opened->surface->info().name;
        co_return pretty(json::Object{{"uri", opened->info.uri.as_str()},
                                      {"title", title},
                                      {"width", std::uint64_t{opened->info.width}},
                                      {"height", std::uint64_t{opened->info.height}}});
    }
};

class Navigate final : public CtxTool<NavigateArgs> {
  public:
    explicit Navigate(Ctx ctx)
        : CtxTool(std::move(ctx), NAVIGATE, "Load a URL in a browser surface and wait for it. Returns the new URL and title.") {}
    Awaitable<ToolResult> run(const CallCtx&, NavigateArgs a) const override {
        auto tab = co_await c.tab(std::move(a.uri));
        if (!tab) co_return std::unexpected(std::move(tab.error()));
        auto info = co_await (*tab)->navigate(normalize_url(a.url));
        if (!info) co_return std::unexpected(info.error().message());
        co_return pretty(info->to_json());
    }
};

class PageText final : public CtxTool<UriArgs> {
  public:
    explicit PageText(Ctx ctx) : CtxTool(std::move(ctx), PAGE_TEXT, "The URL, title and readable text of the page in a browser surface.") {}
    Awaitable<ToolResult> run(const CallCtx&, UriArgs a) const override {
        auto tab = co_await c.tab(std::move(a.uri));
        if (!tab) co_return std::unexpected(std::move(tab.error()));
        auto info = co_await (*tab)->page_info();
        if (!info) co_return std::unexpected(info.error().message());
        auto text = co_await (*tab)->page_text();
        if (!text) co_return std::unexpected(text.error().message());
        co_return ToolOutput::text("URL: " + info->url + "\nTitle: " + info->title + "\n\n" + *text);
    }
};

class Elements final : public CtxTool<UriArgs> {
  public:
    explicit Elements(Ctx ctx)
        : CtxTool(std::move(ctx), ELEMENTS,
                  "The interactive elements of the page in a browser surface (role, name and box in surface pixels), to aim clicks.") {}
    Awaitable<ToolResult> run(const CallCtx&, UriArgs a) const override {
        auto tab = co_await c.tab(std::move(a.uri));
        if (!tab) co_return std::unexpected(std::move(tab.error()));
        auto outline = co_await (*tab)->outline();
        if (!outline) co_return std::unexpected(outline.error().message());
        json::Array nodes;
        for (const auto& node : *outline) nodes.push_back(node.to_json());
        co_return pretty(std::move(nodes));
    }
};

class DocumentData final : public CtxTool<DataArgs> {
  public:
    explicit DocumentData(Ctx ctx)
        : CtxTool(std::move(ctx), DOCUMENT_DATA,
                  "Structured data of a document surface. query.kind is one of: info (any file), text (PDF pages, DOCX, PPTX slides; "
                  "optional page from 0), sheets (XLSX), cells (XLSX; range in A1 notation, optional sheet), outline (DOCX), slides "
                  "(PPTX).") {}
    Awaitable<ToolResult> run(const CallCtx&, DataArgs a) const override {
        auto uri = c.uri(a.uri);
        if (!uri) co_return std::unexpected(std::move(uri.error()));
        auto document = c.hub->document(*uri);
        if (!document) co_return std::unexpected(document.error().message());
        auto data = co_await (*document)->data(std::move(a.query));
        if (!data) co_return std::unexpected(data.error().message());
        co_return pretty(*data);
    }
};

std::string io_text(int code) { return std::string(std::strerror(code)) + " (os error " + std::to_string(code) + ")"; }

} // namespace

ToolSet tool_set(const std::shared_ptr<SurfaceHub>& hub, std::optional<std::string_view> scope) {
    const Ctx c{hub, scope ? std::optional<std::string>(*scope) : std::nullopt};
    const auto within = [&](std::string_view server) { return !scope || *scope == server; };
    ToolSet set;
    set.add(std::make_shared<List>(c)).add(std::make_shared<Screenshot>(c));
    if (scope != std::optional(docs::SERVER_NAME)) {
        set.add(std::make_shared<Click>(c)).add(std::make_shared<Type>(c)).add(std::make_shared<Scroll>(c));
    }
    if (within(browser::SERVER_NAME)) {
        set.add(std::make_shared<OpenBrowser>(c)).add(std::make_shared<Navigate>(c));
        set.add(std::make_shared<PageText>(c)).add(std::make_shared<Elements>(c));
    }
    if (within(docs::SERVER_NAME)) set.add(std::make_shared<DocumentData>(c));
    return set;
}

SurfaceTools::SurfaceTools(std::shared_ptr<SurfaceHub> hub, ToolSet set) : hub_(std::move(hub)), set_(std::move(set)) {}

std::shared_ptr<SurfaceTools> SurfaceTools::make(std::shared_ptr<SurfaceHub> hub) {
    auto set = tool_set(hub, std::nullopt);
    return std::shared_ptr<SurfaceTools>(new SurfaceTools(std::move(hub), std::move(set)));
}

std::pair<std::vector<contracts::HostToolSpec>, std::shared_ptr<const contracts::HostTools>> SurfaceTools::host_tools() const {
    return {set_.specs(), shared_from_this()};
}

Awaitable<ToolOutput> SurfaceTools::run(std::string name, Json args, std::optional<std::string> scope) const {
    const CallCtx call;
    ToolResult result;
    if (!scope) {
        result = co_await set_.call(call, std::move(name), std::move(args));
    } else {
        const ToolSet scoped = tool_set(hub_, *scope);
        result = co_await scoped.call(call, std::move(name), std::move(args));
    }
    co_return result ? std::move(*result) : ToolOutput::error(std::move(result.error()));
}

std::expected<std::filesystem::path, std::string> SurfaceTools::save_screenshot(std::span<const std::byte> png) const {
    const auto& dir = hub_->config().screenshot_dir;
    std::error_code ec;
    std::filesystem::create_directories(dir, ec);
    if (ec) return std::unexpected("saving the screenshot: " + io_text(ec.value()));
    const auto n = shots_.fetch_add(1) + 1;
    auto path = dir / ("surface-" + std::to_string(::getpid()) + "-" + std::to_string(n) + ".png");
    std::FILE* file = std::fopen(path.c_str(), "wb");
    if (!file) return std::unexpected("saving the screenshot: " + io_text(errno));
    const bool written = std::fwrite(png.data(), 1, png.size(), file) == png.size();
    const int error = errno;
    if (std::fclose(file) != 0 || !written) return std::unexpected("saving the screenshot: " + io_text(written ? errno : error));
    return path;
}

Awaitable<std::expected<std::string, std::string>> SurfaceTools::call(std::string session_id, std::string tool, std::string args) const {
    auto blocks = co_await call_content(std::move(session_id), std::move(tool), std::move(args));
    if (!blocks) co_return std::unexpected(std::move(blocks.error()));
    std::string out;
    bool first = true;
    for (const auto& block : *blocks) {
        std::string line;
        if (const auto* text = std::get_if<TextBlock>(&block)) {
            line = text->text;
        } else {
            const auto& image = std::get<ImageBlock>(block);
            auto path = save_screenshot(image.data);
            if (!path) co_return std::unexpected(std::move(path.error()));
            const auto* bytes = reinterpret_cast<const std::uint8_t*>(image.data.data());
            const auto size = png_size(std::span(bytes, image.data.size()));
            const std::string dims = size ? " (" + std::to_string(size->first) + "x" + std::to_string(size->second) + ")" : "";
            line = "Screenshot" + dims + " saved to " + path->string();
        }
        out += (first ? "" : "\n") + line;
        first = false;
    }
    co_return out;
}

Awaitable<std::expected<std::vector<OutputBlock>, std::string>> SurfaceTools::call_content(std::string session_id, std::string tool,
                                                                                          std::string args) const {
    co_return co_await set_.call_content(std::move(session_id), std::move(tool), std::move(args));
}

} // namespace loa::surfaces::hub
