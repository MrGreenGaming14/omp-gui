#include "loa/trust/token.hpp"

#include <algorithm>
#include <format>
#include <limits>

#include "chain.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/text.hpp"

namespace loa::trust {

namespace bsc = loa::biscuit_ffi;
namespace json = loa::runtime::json;

namespace {

TrustError biscuit_error(const bsc::Error& e) { return TrustError::biscuit(e.message); }

Result<bsc::KeyPair> key_pair(const NodeIdentity& identity) {
    auto pair = bsc::KeyPair::from_seed(identity.secret_bytes());
    if (!pair) return std::unexpected(biscuit_error(pair.error()));
    return std::move(*pair);
}

bsc::Result<bsc::PublicKey> biscuit_key(const PublicKey& key) { return bsc::PublicKey::from_bytes(key.as_bytes()); }

// Its kind and the values of its right fact.
std::vector<bsc::Term> right_values(const Resource& resource) {
    struct Visit {
        std::vector<bsc::Term> operator()(const resource::Session& r) const { return {bsc::Term::string(r.id)}; }
        // `validate` checked the id fits an i64.
        std::vector<bsc::Term> operator()(const resource::Prompt& r) const {
            return {bsc::Term::string(r.session), bsc::Term::integer(static_cast<std::int64_t>(r.turn.value))};
        }
        std::vector<bsc::Term> operator()(const resource::Project& r) const { return {bsc::Term::string(r.id)}; }
        std::vector<bsc::Term> operator()(const resource::FutureSessions& r) const { return {bsc::Term::integer(r.after_ms)}; }
        std::vector<bsc::Term> operator()(const resource::Ssh& r) const { return {bsc::Term::string(r.host)}; }
        std::vector<bsc::Term> operator()(const resource::Surface& r) const { return {bsc::Term::string(r.uri)}; }
        std::vector<bsc::Term> operator()(const resource::Sandbox&) const { return {bsc::Term::string(std::string(SANDBOX))}; }
        std::vector<bsc::Term> operator()(const resource::Folder& r) const { return {bsc::Term::string(r.path)}; }
    };
    return std::visit(Visit{}, resource);
}

const std::string* text(std::span<const bsc::Term> values, std::size_t i) {
    return i < values.size() ? std::get_if<std::string>(&values[i].value) : nullptr;
}

const std::int64_t* number(std::span<const bsc::Term> values, std::size_t i) {
    return i < values.size() ? std::get_if<std::int64_t>(&values[i].value) : nullptr;
}

// The resource of kind `index` from its right's values; none when they don't fit.
std::optional<Resource> build_resource(std::size_t index, std::span<const bsc::Term> v) {
    switch (index) {
        case 0:
            if (auto* s = text(v, 0)) return resource::Session{*s};
            break;
        case 1: {
            auto* s = text(v, 0);
            auto* n = number(v, 1);
            if (s && n && *n >= 0) return resource::Prompt{*s, core::NodeId{static_cast<std::uint64_t>(*n)}};
            break;
        }
        case 2:
            if (auto* s = text(v, 0)) return resource::Project{*s};
            break;
        case 3:
            if (auto* n = number(v, 0)) return resource::FutureSessions{*n};
            break;
        case 4:
            if (auto* s = text(v, 0)) return resource::Ssh{*s};
            break;
        case 5:
            if (auto* s = text(v, 0)) return resource::Surface{*s};
            break;
        case 6: return resource::Sandbox{};
        case 7:
            if (auto* s = text(v, 0)) return resource::Folder{*s};
            break;
        default: break;
    }
    return std::nullopt;
}

std::string term_text(const bsc::Term& term) {
    struct Visit {
        std::string operator()(const std::string& s) const { return "\"" + s + "\""; }
        std::string operator()(std::int64_t n) const { return std::to_string(n); }
        std::string operator()(const std::vector<bsc::Term>& set) const {
            std::string out = "{";
            for (std::size_t i = 0; i < set.size(); ++i) out += (i ? ", " : "") + term_text(set[i]);
            return out + "}";
        }
    };
    return std::visit(Visit{}, term.value);
}

std::string fact_text(const bsc::Fact& fact) {
    std::string out = fact.name + "(";
    for (std::size_t i = 0; i < fact.terms.size(); ++i) out += (i ? ", " : "") + term_text(fact.terms[i]);
    return out + ")";
}

bsc::Params expiry_param(std::int64_t expires_ms) { return {{"expires", bsc::Term::integer(expires_ms)}}; }

std::string context_json(const GrantId& grant, const PublicKey& holder, std::int64_t expires_ms, bool reshare) {
    return json::dump(json::Object{
        {"g", grant.value}, {"h", holder.to_string()}, {"e", expires_ms}, {"r", reshare}});
}

struct BlockContext {
    GrantId g;
    std::string h;
    std::int64_t e;
    bool r;
};

std::optional<BlockContext> parse_context(const std::optional<std::string>& text) {
    if (!text) return std::nullopt;
    auto value = json::parse(*text);
    if (!value || !value->is_object()) return std::nullopt;
    const auto* g = value->find("g");
    const auto* h = value->find("h");
    const auto* e = value->find("e");
    const auto* r = value->find("r");
    if (!g || !g->as_string() || !h || !h->as_string() || !e || !e->as_i64() || !r || !r->as_bool()) return std::nullopt;
    return BlockContext{GrantId{*g->as_string()}, *h->as_string(), *e->as_i64(), *r->as_bool()};
}

// One check that passes when the request falls in any of `scopes`. Values go
// in as parameters, so names can't inject Datalog.
std::pair<std::string, bsc::Params> narrowing_check(std::span<const Scope> scopes) {
    bsc::Params params;
    std::vector<std::string> bodies;
    for (std::size_t i = 0; i < scopes.size(); ++i) {
        const auto& scope = scopes[i];
        const auto& kind = kind_of(scope.resource);
        std::vector<bsc::Term> actions;
        for (const auto action : scope.actions) actions.push_back(bsc::Term::string(std::string(as_str(action))));
        params.insert_or_assign(std::format("a{}", i), bsc::Term::set(std::move(actions)));
        auto values = right_values(scope.resource);
        for (std::size_t j = 0; j < values.size(); ++j) params.insert_or_assign(std::format("v{}x{}", i, j), std::move(values[j]));
        const auto body = kind.covers_with([i](std::size_t j) { return std::format("{{v{}x{}}}", i, j); });
        bodies.push_back(std::format("action($a), {}, {{a{}}}.contains($a)", body, i));
    }
    std::string any;
    if (bodies.empty()) any = "false";
    for (std::size_t i = 0; i < bodies.size(); ++i) any += (i ? " or " : "") + bodies[i];
    return {std::format("check if {};", any), std::move(params)};
}

} // namespace

GrantId GrantId::random() { return GrantId{loa::runtime::text::random_hex(16)}; }

std::string Denied::to_string() const {
    switch (kind) {
        case Kind::bad_token: return "the token doesn't parse or isn't signed by this node: " + message;
        case Kind::broken_chain: return std::format("block {} isn't signed by the holder before it", block);
        case Kind::reshare_forbidden: return std::format("block {} re-shares a grant that may not be re-shared", block);
        case Kind::not_holder:
            return std::format("the token was issued to {}, not to {}", holder.to_string(), requester.to_string());
        case Kind::bad_proof: return "the holder's proof doesn't match the challenge";
        case Kind::revoked: return std::format("grant {} is revoked", grant.value);
        case Kind::expired: return std::format("the token expired at {}", expired_ms);
        case Kind::not_granted: return std::format("the grant has no `{}` right on this {}", as_str(action), resource);
        case Kind::narrowed:
            return std::format("re-share {} narrowed `{}` on this {} away", block, as_str(action), resource);
        case Kind::engine: return "authorization failed: " + message;
    }
    return message;
}

namespace detail {

std::int64_t Chain::expires_ms() const {
    if (links.empty()) return std::numeric_limits<std::int64_t>::min();
    return std::ranges::min(links, {}, &Link::expires_ms).expires_ms;
}

std::expected<Chain, Denied> Chain::parse(std::span<const std::uint8_t> token, const PublicKey& root) {
    auto key = biscuit_key(root);
    if (!key) return std::unexpected(Denied::bad_token(key.error().message));
    auto biscuit = bsc::Biscuit::from(token, *key);
    if (!biscuit) return std::unexpected(Denied::bad_token(biscuit.error().message));
    const auto contexts = biscuit->context();
    const auto signers = biscuit->external_public_keys();
    std::vector<Link> links;
    for (std::size_t block = 0; block < std::min(contexts.size(), signers.size()); ++block) {
        const auto broken = Denied::broken_chain(block);
        const auto context = parse_context(contexts[block]);
        if (!context) return std::unexpected(broken);
        const auto holder = PublicKey::from_hex(context->h);
        if (!holder) return std::unexpected(broken);
        PublicKey issuer = root;
        if (!links.empty()) {
            const auto& previous = links.back();
            if (!signers[block]) return std::unexpected(broken);
            const PublicKey signer{signers[block]->bytes()};
            if (signer != previous.holder) return std::unexpected(broken);
            if (!previous.reshare) return std::unexpected(Denied::reshare_forbidden(block));
            issuer = signer;
        }
        links.push_back(Link{context->g, *holder, issuer, context->e, context->r});
    }
    if (links.empty()) return std::unexpected(Denied::broken_chain(0));
    return Chain{std::move(*biscuit), std::move(links)};
}

Result<std::vector<std::uint8_t>> mint(const NodeIdentity& issuer, const GrantId& grant, const PublicKey& holder,
                                       std::span<const Scope> scopes, std::int64_t expires_ms, bool reshare) {
    const auto fail = [](const bsc::Error& e) { return std::unexpected(biscuit_error(e)); };
    bsc::BiscuitBuilder builder;
    for (const auto& scope : scopes) {
        const auto& kind = kind_of(scope.resource);
        const auto values = right_values(scope.resource);
        for (const auto action : scope.actions) {
            bsc::Fact fact{std::string(kind.right), values};
            fact.terms.push_back(bsc::Term::string(std::string(as_str(action))));
            if (auto r = builder.fact(fact); !r) return fail(r.error());
        }
    }
    if (auto r = builder.code(EXPIRY_CHECK, expiry_param(expires_ms)); !r) return fail(r.error());
    if (auto r = builder.context(context_json(grant, holder, expires_ms, reshare)); !r) return fail(r.error());
    auto pair = key_pair(issuer);
    if (!pair) return std::unexpected(pair.error());
    auto token = builder.build(*pair);
    if (!token) return fail(token.error());
    auto bytes = token->to_vec();
    if (!bytes) return fail(bytes.error());
    return std::move(*bytes);
}

Result<std::vector<std::uint8_t>> attenuate(const Chain& parent, const NodeIdentity& holder, const GrantId& grant,
                                            const PublicKey& new_holder, std::optional<std::span<const Scope>> scopes,
                                            std::int64_t expires_ms, bool reshare) {
    const auto fail = [](const bsc::Error& e) { return std::unexpected(biscuit_error(e)); };
    bsc::BlockBuilder block;
    if (auto r = block.code(EXPIRY_CHECK, expiry_param(expires_ms)); !r) return fail(r.error());
    if (scopes) {
        auto [source, params] = narrowing_check(*scopes);
        if (auto r = block.code(source, params); !r) return fail(r.error());
    }
    if (auto r = block.context(context_json(grant, new_holder, expires_ms, reshare)); !r) return fail(r.error());
    auto pair = key_pair(holder);
    if (!pair) return std::unexpected(pair.error());
    auto request = parent.biscuit.third_party_request();
    if (!request) return fail(request.error());
    auto signed_block = std::move(*request).create_block(*pair, block);
    if (!signed_block) return fail(signed_block.error());
    auto appended = parent.biscuit.append_third_party(pair->public_key(), *signed_block);
    if (!appended) return fail(appended.error());
    auto bytes = appended->to_vec();
    if (!bytes) return fail(bytes.error());
    return std::move(*bytes);
}

} // namespace detail

Result<std::vector<Scope>> token_scopes(std::span<const std::uint8_t> token, const PublicKey& root) {
    const auto fail = [](const bsc::Error& e) { return std::unexpected(biscuit_error(e)); };
    auto key = biscuit_key(root);
    if (!key) return fail(key.error());
    auto biscuit = bsc::Biscuit::from(token, *key);
    if (!biscuit) return fail(biscuit.error());
    auto authorizer = bsc::AuthorizerBuilder().build(*biscuit);
    if (!authorizer) return fail(authorizer.error());
    std::vector<Scope> scopes;
    for (std::size_t index = 0; index < KINDS.size(); ++index) {
        const auto& kind = KINDS[index];
        const auto vars = kind.vars();
        auto rights = authorizer->query(std::format("r({}$a) <- {}({}$a)", vars, kind.right, vars));
        if (!rights) return fail(rights.error());
        for (const auto& right : *rights) {
            const auto malformed = [&] {
                return std::unexpected(TrustError::malformed(std::format("bad {} right {}", kind.name, fact_text(right))));
            };
            if (right.terms.empty()) return malformed();
            const auto* action_text = std::get_if<std::string>(&right.terms.back().value);
            const auto action = action_text ? parse_action(*action_text) : std::nullopt;
            if (!action) return malformed();
            auto resource = build_resource(index, std::span(right.terms).first(right.terms.size() - 1));
            if (!resource) return malformed();
            const auto found = std::ranges::find(scopes, *resource, &Scope::resource);
            if (found != scopes.end()) found->actions.insert(*action);
            else scopes.push_back(Scope{std::move(*resource), ActionSet{*action}});
        }
    }
    return scopes;
}

const std::string& rules() {
    static const std::string text = [] {
        std::string out;
        for (const auto& kind : KINDS) {
            const auto covers = kind.covers_with([](std::size_t i) { return std::format("$v{}", i); });
            out += std::format("granted($a) <- {}({}$a), {};\n", kind.right, kind.vars(), covers);
        }
        return out + "allow if action($a), granted($a);";
    }();
    return text;
}

std::expected<VerifiedGrant, Denied> Verifier::verify(Presented presented, const PublicKey& requester,
                                                      const Challenge& challenge, std::int64_t now_ms) const {
    auto chain = detail::Chain::parse(presented.token, root_);
    if (!chain) return std::unexpected(std::move(chain.error()));
    const auto holder = chain->last().holder;
    if (holder != requester) return std::unexpected(Denied::not_holder(holder, requester));
    if (presented.proof == nullptr || !proof_valid(requester, challenge, presented.token, *presented.proof))
        return std::unexpected(Denied::bad_proof());
    for (const auto& link : chain->links)
        if (revocations_->is_revoked(link.grant)) return std::unexpected(Denied::revoked(link.grant));
    VerifiedGrant grant(std::make_shared<const detail::Chain>(std::move(*chain)));
    if (now_ms >= grant.expires_ms()) return std::unexpected(Denied::expired(grant.expires_ms()));
    return grant;
}

const std::vector<Link>& VerifiedGrant::chain() const noexcept { return chain_->links; }

std::int64_t VerifiedGrant::expires_ms() const noexcept { return chain_->expires_ms(); }

std::expected<void, Denied> VerifiedGrant::live(std::int64_t now_ms) const {
    const auto expired_ms = expires_ms();
    if (now_ms >= expired_ms) return std::unexpected(Denied::expired(expired_ms));
    return {};
}

std::expected<void, Denied> VerifiedGrant::authorize(const Request& request, std::int64_t now_ms) const {
    if (auto ok = live(now_ms); !ok) return ok;
    const auto action = request.action;
    std::string_view resource;
    if (const auto* s = std::get_if<target::Session>(&request.target)) resource = s->turn ? "prompt" : "session";
    else if (std::holds_alternative<target::Ssh>(request.target)) resource = "ssh host";
    else if (std::holds_alternative<target::Surface>(request.target)) resource = "surface";
    else if (std::holds_alternative<target::Sandbox>(request.target)) resource = "sandbox";
    else resource = "folder";
    const auto not_granted = Denied::not_granted(action, resource);

    const auto str = [](std::string_view s) { return bsc::Term::string(std::string(s)); };
    std::vector<bsc::Fact> facts{{"action", {str(as_str(action))}}, {"time_ms", {bsc::Term::integer(now_ms)}}};
    if (const auto* s = std::get_if<target::Session>(&request.target)) {
        facts.push_back({"session", {str(s->session->id)}});
        facts.push_back({"created", {bsc::Term::integer(s->session->created_ms)}});
        if (s->session->project) facts.push_back({"project", {str(*s->session->project)}});
        if (s->turn) {
            // Ids above i64::MAX are in no grant (see `Scope::make`).
            if (s->turn->value > static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max()))
                return std::unexpected(not_granted);
            facts.push_back({"turn", {bsc::Term::integer(static_cast<std::int64_t>(s->turn->value))}});
        }
    } else if (const auto* h = std::get_if<target::Ssh>(&request.target)) {
        facts.push_back({"ssh_host", {str(h->host)}});
    } else if (const auto* u = std::get_if<target::Surface>(&request.target)) {
        facts.push_back({"surface", {str(u->uri)}});
    } else if (std::holds_alternative<target::Sandbox>(request.target)) {
        facts.push_back({"sandbox", {str(SANDBOX)}});
    } else {
        // The folder and every folder above it: a grant on any covers it.
        const auto ancestors = folder_ancestors(std::get<target::Folder>(request.target).path);
        if (!ancestors) return std::unexpected(not_granted);
        for (const auto& folder : *ancestors) facts.push_back({"folder", {str(folder)}});
    }

    const auto engine = [](const bsc::Error& e) { return std::unexpected(Denied::engine(e.message)); };
    bsc::AuthorizerBuilder builder;
    if (auto r = builder.code(rules()); !r) return engine(r.error());
    if (auto r = builder.set_limits({1000, 100, std::chrono::milliseconds(250)}); !r) return engine(r.error());
    for (const auto& f : facts)
        if (auto r = builder.fact(f); !r) return engine(r.error());
    auto authorizer = builder.build(chain_->biscuit);
    if (!authorizer) return engine(authorizer.error());
    auto result = authorizer->authorize();
    if (result) return {};
    const auto& error = result.error();
    // The allow policy didn't match: the original grant lacks the right.
    if (error.status == bsc::Status::no_matching_policy) return std::unexpected(not_granted);
    // Expiry was checked above, so a failed re-share block check is its
    // narrowing; a failed authority check is expiry.
    if (error.status == bsc::Status::unauthorized) {
        for (const auto& check : error.checks)
            if (check.is_block && check.block_id > 0) return std::unexpected(Denied::narrowed(action, resource, check.block_id));
        return std::unexpected(Denied::expired(expires_ms()));
    }
    return engine(error);
}

} // namespace loa::trust
