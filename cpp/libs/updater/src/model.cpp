#include "loa/updater/model.hpp"

#include "util.hpp"

namespace loa::updater {

using namespace detail;

Json to_json(const Feed& v) {
    if (v.type == Feed::Type::GitHub) return Object{{"type", "github"}, {"owner", v.owner}, {"repo", v.repo}};
    return Object{{"type", "npm"}, {"package", v.package}};
}

Json to_json(PrereleasePolicy v) { return v == PrereleasePolicy::Stable ? "stable" : "include"; }

Json to_json(const Product& v) {
    return Object{{"id", v.id},
                  {"kind", loa::settings::to_json(v.kind)},
                  {"feed", to_json(v.feed)},
                  {"prereleases", to_json(v.prereleases)},
                  {"asset", opt(v.asset)}};
}

Json to_json(const CachedFeed& v) {
    Array releases;
    for (const auto& r : v.releases) releases.push_back(loa::settings::to_json(r));
    return Object{{"etag", opt(v.etag)}, {"releases", std::move(releases)}};
}

Json to_json(const Installed& v) {
    if (v.type == Installed::Type::Running) return Object{{"running", v.version.to_string()}};
    return Object{{"executable", v.path.string()}};
}

Json to_json(const Gates& v) {
    switch (v.type) {
        case Gates::Type::None: return Object{{"type", "none"}};
        case Gates::Type::Changelog: return Object{{"type", "changelog"}};
        case Gates::Type::Omp: break;
    }
    Object out{{"type", "omp"}};
    const Json fields = to_json(v.omp);
    for (const auto& entry : *fields.as_object()) out.push_back(entry);
    return out;
}

Json to_json(const ProductSpec& v) {
    return Object{{"product", to_json(v.product)},
                  {"label", v.label},
                  {"installed", to_json(v.installed)},
                  {"executable", opt(v.executable)},
                  {"gates", to_json(v.gates)}};
}

namespace {

Feed feed_value(const Json& v) {
    const auto& o = object(v, "internally tagged enum Feed");
    const auto type = string(required(o, "type"));
    if (type == "github") return Feed::github(string(required(o, "owner")), string(required(o, "repo")));
    if (type == "npm") return Feed::npm(string(required(o, "package")));
    throw DecodeError{"unknown variant `" + type + "`, expected `github` or `npm`"};
}

PrereleasePolicy policy_value(const Json* v) {
    if (!v) return PrereleasePolicy::Stable;
    const auto s = string(*v);
    if (s == "stable") return PrereleasePolicy::Stable;
    if (s == "include") return PrereleasePolicy::Include;
    throw DecodeError{"unknown variant `" + s + "`, expected `stable` or `include`"};
}

Product product_value(const Json& v) {
    const auto& o = object(v, "struct Product");
    Product p;
    p.id = string(required(o, "id"));
    p.kind = settings_value<ProductKind>(required(o, "kind"));
    p.feed = feed_value(required(o, "feed"));
    p.prereleases = policy_value(field(o, "prereleases"));
    p.asset = opt_string(field(o, "asset"));
    return p;
}

Installed installed_value(const Json& v) {
    const auto& o = object(v, "enum Installed");
    if (o.size() != 1) throw DecodeError{"invalid type: expected enum Installed"};
    const auto& [tag, inner] = o.front();
    if (tag == "running") return Installed::running(version(inner));
    if (tag == "executable") return Installed::executable(string(inner));
    throw DecodeError{"unknown variant `" + tag + "`, expected `running` or `executable`"};
}

Gates gates_value(const Json& v) {
    const auto& o = object(v, "internally tagged enum Gates");
    const auto type = string(required(o, "type"));
    if (type == "none") return Gates::none();
    if (type == "changelog") return Gates::changelog();
    if (type == "omp") return Gates::omp_gates(value(omp_gates_from_json(v)));
    throw DecodeError{"unknown variant `" + type + "`, expected one of `none`, `changelog`, `omp`"};
}

} // namespace

Decoded<Feed> feed_from_json(const Json& v) {
    return decoding([&] { return feed_value(v); });
}

Decoded<Product> product_from_json(const Json& v) {
    return decoding([&] { return product_value(v); });
}

Decoded<CachedFeed> cached_feed_from_json(const Json& v) {
    return decoding([&] {
        const auto& o = object(v, "struct CachedFeed");
        CachedFeed out;
        out.etag = opt_string(field(o, "etag"));
        for (const auto& r : array(required(o, "releases"))) out.releases.push_back(settings_value<Release>(r));
        return out;
    });
}

Decoded<Installed> installed_from_json(const Json& v) {
    return decoding([&] { return installed_value(v); });
}

Decoded<Gates> gates_from_json(const Json& v) {
    return decoding([&] { return gates_value(v); });
}

Decoded<ProductSpec> product_spec_from_json(const Json& v) {
    return decoding([&] {
        const auto& o = object(v, "struct ProductSpec");
        ProductSpec s;
        s.product = product_value(required(o, "product"));
        s.label = string(required(o, "label"));
        s.installed = installed_value(required(o, "installed"));
        s.executable = opt_string(field(o, "executable"));
        s.gates = gates_value(required(o, "gates"));
        return s;
    });
}

} // namespace loa::updater
