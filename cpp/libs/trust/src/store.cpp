#include "loa/trust/store.hpp"

#include <sqlite3.h>

#include <algorithm>
#include <format>
#include <functional>
#include <mutex>

#include "chain.hpp"
#include "loa/runtime/json.hpp"

namespace loa::trust {

namespace json = loa::runtime::json;

const std::string_view GrantStore::SCHEMA = R"(
CREATE TABLE IF NOT EXISTS grants_issued (id TEXT PRIMARY KEY, holder TEXT NOT NULL, spec TEXT NOT NULL, parent TEXT,
    created_ms INTEGER NOT NULL, revoked_ms INTEGER);
CREATE TABLE IF NOT EXISTS grants_received (id TEXT PRIMARY KEY, issuer TEXT NOT NULL, root TEXT NOT NULL, token BLOB NOT NULL,
    expires_ms INTEGER NOT NULL, reshare INTEGER NOT NULL, refused_ms INTEGER);
CREATE TABLE IF NOT EXISTS grants_revocations (id TEXT PRIMARY KEY, issuer TEXT NOT NULL, revoked_ms INTEGER NOT NULL);
)";

namespace {

constexpr std::string_view ISSUED = "SELECT id, holder, spec, parent, created_ms, revoked_ms FROM grants_issued";
constexpr std::string_view RECEIVED =
    "SELECT id, issuer, root, token, expires_ms, reshare, refused_ms FROM grants_received";

// ---- JSON forms ------------------------------------------------------------

TrustError missing(std::string_view field) { return TrustError::json(std::format("missing field `{}`", field)); }

Result<std::string> str_field(const json::Value& v, std::string_view name) {
    const auto* f = v.find(name);
    if (!f) return std::unexpected(missing(name));
    if (!f->as_string()) return std::unexpected(TrustError::json(std::format("`{}` is not a string", name)));
    return *f->as_string();
}

Result<std::int64_t> int_field(const json::Value& v, std::string_view name) {
    const auto* f = v.find(name);
    if (!f) return std::unexpected(missing(name));
    if (!f->as_i64()) return std::unexpected(TrustError::json(std::format("`{}` is not an i64", name)));
    return *f->as_i64();
}

Result<bool> bool_field(const json::Value& v, std::string_view name) {
    const auto* f = v.find(name);
    if (!f) return std::unexpected(missing(name));
    if (!f->as_bool()) return std::unexpected(TrustError::json(std::format("`{}` is not a bool", name)));
    return *f->as_bool();
}

json::Value resource_to_json(const Resource& resource) {
    struct Visit {
        json::Object operator()(const resource::Session& r) const { return {{"kind", "session"}, {"id", r.id}}; }
        json::Object operator()(const resource::Prompt& r) const {
            return {{"kind", "prompt"}, {"session", r.session}, {"turn", r.turn.value}};
        }
        json::Object operator()(const resource::Project& r) const { return {{"kind", "project"}, {"id", r.id}}; }
        json::Object operator()(const resource::FutureSessions& r) const {
            return {{"kind", "future_sessions"}, {"after_ms", r.after_ms}};
        }
        json::Object operator()(const resource::Ssh& r) const { return {{"kind", "ssh"}, {"host", r.host}}; }
        json::Object operator()(const resource::Surface& r) const { return {{"kind", "surface"}, {"uri", r.uri}}; }
        json::Object operator()(const resource::Sandbox&) const { return {{"kind", "sandbox"}}; }
        json::Object operator()(const resource::Folder& r) const { return {{"kind", "folder"}, {"path", r.path}}; }
    };
    return std::visit(Visit{}, resource);
}

Result<Resource> resource_from_json(const json::Value& v) {
    if (!v.is_object()) return std::unexpected(TrustError::json("resource is not an object"));
    auto kind = str_field(v, "kind");
    if (!kind) return std::unexpected(kind.error());
    const auto text = [&](std::string_view name) { return str_field(v, name); };
    const auto wrap = [](auto&& field, auto make) -> Result<Resource> {
        if (!field) return std::unexpected(field.error());
        return make(std::move(*field));
    };
    if (*kind == "session") return wrap(text("id"), [](std::string s) { return Resource{resource::Session{s}}; });
    if (*kind == "prompt") {
        auto session = text("session");
        if (!session) return std::unexpected(session.error());
        const auto* turn = v.find("turn");
        if (!turn) return std::unexpected(missing("turn"));
        if (!turn->as_u64()) return std::unexpected(TrustError::json("`turn` is not a u64"));
        return Resource{resource::Prompt{std::move(*session), core::NodeId{*turn->as_u64()}}};
    }
    if (*kind == "project") return wrap(text("id"), [](std::string s) { return Resource{resource::Project{s}}; });
    if (*kind == "future_sessions")
        return wrap(int_field(v, "after_ms"), [](std::int64_t n) { return Resource{resource::FutureSessions{n}}; });
    if (*kind == "ssh") return wrap(text("host"), [](std::string s) { return Resource{resource::Ssh{s}}; });
    if (*kind == "surface") return wrap(text("uri"), [](std::string s) { return Resource{resource::Surface{s}}; });
    if (*kind == "sandbox") return Resource{resource::Sandbox{}};
    if (*kind == "folder") return wrap(text("path"), [](std::string s) { return Resource{resource::Folder{s}}; });
    return std::unexpected(TrustError::json(std::format("unknown variant `{}`", *kind)));
}

json::Value scopes_to_json(const std::vector<Scope>& scopes) {
    json::Array out;
    for (const auto& scope : scopes) out.push_back(scope_to_json(scope));
    return out;
}

Result<std::vector<Scope>> scopes_from_json(const json::Value& v) {
    const auto* array = v.as_array();
    if (!array) return std::unexpected(TrustError::json("`scopes` is not an array"));
    std::vector<Scope> out;
    for (const auto& item : *array) {
        auto scope = scope_from_json(item);
        if (!scope) return std::unexpected(scope.error());
        out.push_back(std::move(*scope));
    }
    return out;
}

// ---- SQLite ----------------------------------------------------------------

TrustError sql_error(sqlite3* db) { return TrustError::sql(sqlite3_errmsg(db)); }

using Bind = std::variant<std::nullptr_t, std::int64_t, std::string_view, std::span<const std::uint8_t>>;

struct Statement {
    sqlite3_stmt* stmt = nullptr;
    Statement() = default;
    Statement(const Statement&) = delete;
    Statement& operator=(const Statement&) = delete;
    ~Statement() { sqlite3_finalize(stmt); }
};

class Row {
  public:
    explicit Row(sqlite3_stmt* s) : s_(s) {}
    [[nodiscard]] bool is_null(int i) const { return sqlite3_column_type(s_, i) == SQLITE_NULL; }
    [[nodiscard]] std::string text(int i) const {
        const auto* p = sqlite3_column_text(s_, i);
        return p ? std::string(reinterpret_cast<const char*>(p), static_cast<std::size_t>(sqlite3_column_bytes(s_, i))) : std::string();
    }
    [[nodiscard]] std::optional<std::string> opt_text(int i) const { return is_null(i) ? std::nullopt : std::optional(text(i)); }
    [[nodiscard]] std::int64_t integer(int i) const { return sqlite3_column_int64(s_, i); }
    [[nodiscard]] std::optional<std::int64_t> opt_integer(int i) const {
        return is_null(i) ? std::nullopt : std::optional(integer(i));
    }
    [[nodiscard]] std::vector<std::uint8_t> blob(int i) const {
        const auto* p = static_cast<const std::uint8_t*>(sqlite3_column_blob(s_, i));
        return p ? std::vector<std::uint8_t>(p, p + sqlite3_column_bytes(s_, i)) : std::vector<std::uint8_t>{};
    }
    [[nodiscard]] Result<PublicKey> key(int i) const {
        auto key = PublicKey::from_hex(text(i));
        if (!key) return std::unexpected(TrustError::sql(std::format("conversion error from type Text at index: {}, {}", i, key.error().to_string())));
        return *key;
    }

  private:
    sqlite3_stmt* s_;
};

Result<void> prepare(sqlite3* db, std::string_view sql, std::initializer_list<Bind> binds, Statement& out) {
    if (sqlite3_prepare_v2(db, sql.data(), static_cast<int>(sql.size()), &out.stmt, nullptr) != SQLITE_OK)
        return std::unexpected(sql_error(db));
    int index = 1;
    for (const auto& bind : binds) {
        int rc = SQLITE_OK;
        if (std::holds_alternative<std::nullptr_t>(bind)) rc = sqlite3_bind_null(out.stmt, index);
        else if (const auto* n = std::get_if<std::int64_t>(&bind)) rc = sqlite3_bind_int64(out.stmt, index, *n);
        else if (const auto* s = std::get_if<std::string_view>(&bind))
            rc = sqlite3_bind_text(out.stmt, index, s->data(), static_cast<int>(s->size()), SQLITE_TRANSIENT);
        else {
            const auto& b = std::get<std::span<const std::uint8_t>>(bind);
            rc = sqlite3_bind_blob(out.stmt, index, b.data(), static_cast<int>(b.size()), SQLITE_TRANSIENT);
        }
        if (rc != SQLITE_OK) return std::unexpected(sql_error(db));
        ++index;
    }
    return {};
}

// Runs `sql` to completion; the number of rows changed.
Result<int> execute(sqlite3* db, std::string_view sql, std::initializer_list<Bind> binds) {
    Statement st;
    if (auto ok = prepare(db, sql, binds, st); !ok) return std::unexpected(ok.error());
    int rc;
    while ((rc = sqlite3_step(st.stmt)) == SQLITE_ROW) {
    }
    if (rc != SQLITE_DONE) return std::unexpected(sql_error(db));
    return sqlite3_changes(db);
}

// Calls `each` per row; stops at its first error.
Result<void> query(sqlite3* db, std::string_view sql, std::initializer_list<Bind> binds,
                   const std::function<Result<void>(const Row&)>& each) {
    Statement st;
    if (auto ok = prepare(db, sql, binds, st); !ok) return std::unexpected(ok.error());
    int rc;
    while ((rc = sqlite3_step(st.stmt)) == SQLITE_ROW) {
        if (auto ok = each(Row(st.stmt)); !ok) return ok;
    }
    if (rc != SQLITE_DONE) return std::unexpected(sql_error(db));
    return {};
}

Result<ReceivedGrant> received_from_row(const Row& r) {
    auto issuer = r.key(1);
    if (!issuer) return std::unexpected(issuer.error());
    auto root = r.key(2);
    if (!root) return std::unexpected(root.error());
    return ReceivedGrant{GrantId{r.text(0)}, *issuer, *root, r.blob(3), r.integer(4), r.integer(5) != 0, r.opt_integer(6)};
}

// One `grants_issued` row.
struct IssuedRow {
    GrantId id;
    PublicKey holder;
    Definition definition;
    std::optional<GrantId> parent;
    std::int64_t created_ms;
    std::optional<std::int64_t> revoked_ms;
};

Result<IssuedRow> issued_from_row(const Row& r) {
    auto holder = r.key(1);
    if (!holder) return std::unexpected(holder.error());
    auto definition = definition_from_json(r.text(2));
    if (!definition)
        return std::unexpected(TrustError::sql(std::format("conversion error from type Text at index: 2, {}", definition.error().to_string())));
    std::optional<GrantId> parent;
    if (auto p = r.opt_text(3)) parent = GrantId{std::move(*p)};
    return IssuedRow{GrantId{r.text(0)}, *holder, std::move(*definition), std::move(parent), r.integer(4), r.opt_integer(5)};
}

Result<void> check_ttl(std::int64_t ttl_ms) {
    if (ttl_ms <= 0) return std::unexpected(TrustError::ttl(ttl_ms));
    return {};
}

Result<void> validate_all(std::span<const Scope> scopes) {
    for (const auto& scope : scopes)
        if (auto ok = scope.validate(); !ok) return ok;
    return {};
}

Result<std::int64_t> token_expiry(const GrantSpec& spec, const GrantId& id, std::int64_t now_ms) {
    const auto expires_ms = detail::saturating_add(now_ms, spec.ttl_ms);
    if (!spec.until_ms) return expires_ms;
    if (*spec.until_ms <= now_ms) return std::unexpected(TrustError::expired(id.value));
    return std::min(expires_ms, *spec.until_ms);
}

Result<IssuedGrant> mint_root(const NodeIdentity& issuer, const GrantId& id, const PublicKey& holder,
                              const GrantSpec& spec, std::int64_t now_ms) {
    if (auto ok = validate_all(spec.scopes); !ok) return std::unexpected(ok.error());
    auto expires_ms = token_expiry(spec, id, now_ms);
    if (!expires_ms) return std::unexpected(expires_ms.error());
    auto token = detail::mint(issuer, id, holder, spec.scopes, *expires_ms, spec.reshare);
    if (!token) return std::unexpected(token.error());
    return IssuedGrant{id, holder, std::move(*token), *expires_ms};
}

std::string_view sv(const std::string& s) { return s; }

} // namespace

json::Value scope_to_json(const Scope& scope) {
    json::Array actions;
    for (const auto action : scope.actions) actions.emplace_back(as_str(action));
    return json::Object{{"resource", resource_to_json(scope.resource)}, {"actions", std::move(actions)}};
}

Result<Scope> scope_from_json(const json::Value& value) {
    if (!value.is_object()) return std::unexpected(TrustError::json("scope is not an object"));
    const auto* r = value.find("resource");
    if (!r) return std::unexpected(missing("resource"));
    auto resource = resource_from_json(*r);
    if (!resource) return std::unexpected(resource.error());
    const auto* a = value.find("actions");
    if (!a) return std::unexpected(missing("actions"));
    if (!a->as_array()) return std::unexpected(TrustError::json("`actions` is not an array"));
    ActionSet actions;
    for (const auto& item : *a->as_array()) {
        const auto action = item.as_string() ? parse_action(*item.as_string()) : std::nullopt;
        if (!action) return std::unexpected(TrustError::json("unknown action"));
        actions.insert(*action);
    }
    return Scope{std::move(*resource), std::move(actions)};
}

std::string definition_to_json(const Definition& definition) {
    if (const auto* spec = std::get_if<GrantSpec>(&definition)) {
        return json::dump(json::Object{{"kind", "root"},
                                       {"scopes", scopes_to_json(spec->scopes)},
                                       {"ttl_ms", spec->ttl_ms},
                                       {"reshare", spec->reshare},
                                       {"until_ms", spec->until_ms ? json::Value(*spec->until_ms) : json::Value()}});
    }
    const auto& n = std::get<Narrowing>(definition);
    return json::dump(json::Object{{"kind", "reshare"},
                                   {"scopes", n.scopes ? scopes_to_json(*n.scopes) : json::Value()},
                                   {"ttl_ms", n.ttl_ms},
                                   {"reshare", n.reshare}});
}

Result<Definition> definition_from_json(std::string_view text) {
    auto value = json::parse(text);
    if (!value) return std::unexpected(TrustError::json(value.error().message()));
    if (!value->is_object()) return std::unexpected(TrustError::json("definition is not an object"));
    auto kind = str_field(*value, "kind");
    if (!kind) return std::unexpected(kind.error());
    auto ttl_ms = int_field(*value, "ttl_ms");
    if (!ttl_ms) return std::unexpected(ttl_ms.error());
    auto reshare = bool_field(*value, "reshare");
    if (!reshare) return std::unexpected(reshare.error());
    const auto* scopes = value->find("scopes");
    if (*kind == "root") {
        if (!scopes) return std::unexpected(missing("scopes"));
        auto list = scopes_from_json(*scopes);
        if (!list) return std::unexpected(list.error());
        std::optional<std::int64_t> until_ms;
        if (const auto* until = value->find("until_ms"); until && !until->is_null()) {
            if (!until->as_i64()) return std::unexpected(TrustError::json("`until_ms` is not an i64"));
            until_ms = *until->as_i64();
        }
        return GrantSpec{std::move(*list), *ttl_ms, *reshare, until_ms};
    }
    if (*kind == "reshare") {
        std::optional<std::vector<Scope>> list;
        if (scopes && !scopes->is_null()) {
            auto parsed = scopes_from_json(*scopes);
            if (!parsed) return std::unexpected(parsed.error());
            list = std::move(*parsed);
        }
        return Narrowing{std::move(list), *ttl_ms, *reshare};
    }
    return std::unexpected(TrustError::json(std::format("unknown variant `{}`", *kind)));
}

struct GrantStore::Impl {
    mutable std::mutex lock;
    sqlite3* db = nullptr;
    ~Impl() { sqlite3_close(db); }
};

GrantStore::GrantStore(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
GrantStore::GrantStore(GrantStore&&) noexcept = default;
GrantStore& GrantStore::operator=(GrantStore&&) noexcept = default;
GrantStore::~GrantStore() = default;

sqlite3* GrantStore::connection() const noexcept { return impl_->db; }

Result<GrantStore> GrantStore::open(const std::filesystem::path& path) {
    sqlite3* db = nullptr;
    const auto rc = sqlite3_open(path.string().c_str(), &db);
    if (rc != SQLITE_OK) {
        auto error = TrustError::sql(db ? sqlite3_errmsg(db) : sqlite3_errstr(rc));
        sqlite3_close(db);
        return std::unexpected(std::move(error));
    }
    return with_connection(db);
}

Result<GrantStore> GrantStore::open_in_memory() { return open(":memory:"); }

Result<GrantStore> GrantStore::with_connection(sqlite3* db) {
    auto impl = std::make_unique<Impl>();
    impl->db = db;
    const std::string schema(SCHEMA);
    if (sqlite3_exec(db, schema.c_str(), nullptr, nullptr, nullptr) != SQLITE_OK) return std::unexpected(sql_error(db));
    return GrantStore(std::move(impl));
}

Result<IssuedGrant> GrantStore::insert_issued(const IssuedGrant& issued, const Definition& definition,
                                              const GrantId* parent, std::int64_t now_ms) const {
    const auto holder = issued.holder.to_string();
    const auto spec = definition_to_json(definition);
    std::scoped_lock guard(impl_->lock);
    auto ok = execute(impl_->db,
                      "INSERT INTO grants_issued (id, holder, spec, parent, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                      {sv(issued.id.value), sv(holder), sv(spec), parent ? Bind(sv(parent->value)) : Bind(nullptr), now_ms});
    if (!ok) return std::unexpected(ok.error());
    return issued;
}

Result<IssuedGrant> GrantStore::issue(const NodeIdentity& issuer, const PublicKey& holder, GrantSpec spec,
                                      std::int64_t now_ms) const {
    if (spec.scopes.empty()) return std::unexpected(TrustError::no_scopes());
    if (auto ok = check_ttl(spec.ttl_ms); !ok) return std::unexpected(ok.error());
    auto issued = mint_root(issuer, GrantId::random(), holder, spec, now_ms);
    if (!issued) return issued;
    return insert_issued(*issued, Definition{std::move(spec)}, nullptr, now_ms);
}

namespace {

// The holder, definition and parent of an active grant this node issued.
Result<IssuedRow> active(sqlite3* db, std::mutex& lock, const GrantId& id) {
    std::optional<IssuedRow> row;
    {
        std::scoped_lock guard(lock);
        auto ok = query(db, std::string(ISSUED) + " WHERE id = ?1", {sv(id.value)}, [&](const Row& r) -> Result<void> {
            auto parsed = issued_from_row(r);
            if (!parsed) return std::unexpected(parsed.error());
            row = std::move(*parsed);
            return {};
        });
        if (!ok) return std::unexpected(ok.error());
    }
    if (!row) return std::unexpected(TrustError::unknown_grant(id.value));
    if (row->revoked_ms) return std::unexpected(TrustError::revoked(id.value));
    return std::move(*row);
}

} // namespace

Result<IssuedGrant> GrantStore::renew(const NodeIdentity& issuer, const GrantId& id, const PublicKey& requester,
                                      std::int64_t now_ms) const {
    auto row = active(impl_->db, impl_->lock, id);
    if (!row) return std::unexpected(row.error());
    if (row->holder != requester) return std::unexpected(TrustError::not_holder(id.value));
    if (const auto* spec = std::get_if<GrantSpec>(&row->definition); spec && !row->parent)
        return mint_root(issuer, id, row->holder, *spec, now_ms);
    if (const auto* narrowing = std::get_if<Narrowing>(&row->definition); narrowing && row->parent)
        return attenuate_from(issuer, *row->parent, id, row->holder, *narrowing, now_ms);
    return std::unexpected(TrustError::malformed(std::format("grant {} has a parent only when it is a re-share", id.value)));
}

Result<IssuedGrant> GrantStore::extend(const NodeIdentity& issuer, const GrantId& id,
                                       std::optional<std::int64_t> until_ms, std::int64_t now_ms) const {
    auto row = active(impl_->db, impl_->lock, id);
    if (!row) return std::unexpected(row.error());
    auto* spec = std::get_if<GrantSpec>(&row->definition);
    if (!spec || row->parent)
        return std::unexpected(TrustError::malformed(std::format("only root grants can be extended, {} is a re-share", id.value)));
    spec->until_ms = until_ms;
    auto issued = mint_root(issuer, id, row->holder, *spec, now_ms);
    if (!issued) return issued;
    const auto text = definition_to_json(row->definition);
    std::scoped_lock guard(impl_->lock);
    auto ok = execute(impl_->db, "UPDATE grants_issued SET spec = ?2 WHERE id = ?1", {sv(id.value), sv(text)});
    if (!ok) return std::unexpected(ok.error());
    return issued;
}

Result<IssuedGrant> GrantStore::reshare(const NodeIdentity& holder, const GrantId& parent, const PublicKey& new_holder,
                                        Narrowing narrowing, std::int64_t now_ms) const {
    if (auto ok = check_ttl(narrowing.ttl_ms); !ok) return std::unexpected(ok.error());
    if (narrowing.scopes) {
        if (narrowing.scopes->empty()) return std::unexpected(TrustError::no_scopes());
        if (auto ok = validate_all(*narrowing.scopes); !ok) return std::unexpected(ok.error());
    }
    auto issued = attenuate_from(holder, parent, GrantId::random(), new_holder, narrowing, now_ms);
    if (!issued) return issued;
    return insert_issued(*issued, Definition{std::move(narrowing)}, &parent, now_ms);
}

Result<IssuedGrant> GrantStore::attenuate_from(const NodeIdentity& holder, const GrantId& parent, GrantId id,
                                               const PublicKey& new_holder, const Narrowing& narrowing,
                                               std::int64_t now_ms) const {
    auto found = received(parent);
    if (!found) return std::unexpected(found.error());
    if (!*found) return std::unexpected(TrustError::unknown_grant(parent.value));
    const auto& grant = **found;
    if (!grant.reshare) return std::unexpected(TrustError::reshare_forbidden(parent.value));
    if (grant.expires_ms <= now_ms) return std::unexpected(TrustError::parent_expired(parent.value));
    auto chain = detail::Chain::parse(grant.token, grant.root);
    if (!chain) return std::unexpected(TrustError::malformed(chain.error().to_string()));
    if (chain->last().holder != holder.public_key()) return std::unexpected(TrustError::not_holder(parent.value));
    const auto expires_ms = std::min(detail::saturating_add(now_ms, narrowing.ttl_ms), chain->expires_ms());
    std::optional<std::span<const Scope>> scopes;
    if (narrowing.scopes) scopes = std::span<const Scope>(*narrowing.scopes);
    auto token = detail::attenuate(*chain, holder, id, new_holder, scopes, expires_ms, narrowing.reshare);
    if (!token) return std::unexpected(token.error());
    return IssuedGrant{std::move(id), new_holder, std::move(*token), expires_ms};
}

Result<void> GrantStore::revoke(const PublicKey& issuer, const GrantId& id, std::int64_t now_ms) const {
    Result<int> changed;
    {
        std::scoped_lock guard(impl_->lock);
        changed = execute(impl_->db, "UPDATE grants_issued SET revoked_ms = ?2 WHERE id = ?1 AND revoked_ms IS NULL",
                          {sv(id.value), now_ms});
    }
    if (!changed) return std::unexpected(changed.error());
    if (*changed > 0) return record_revocation(id, issuer, now_ms);
    bool known = false;
    {
        std::scoped_lock guard(impl_->lock);
        auto ok = query(impl_->db, "SELECT EXISTS(SELECT 1 FROM grants_issued WHERE id = ?1)", {sv(id.value)},
                        [&](const Row& r) -> Result<void> {
                            known = r.integer(0) != 0;
                            return {};
                        });
        if (!ok) return ok;
    }
    if (known) return {};
    return std::unexpected(TrustError::unknown_grant(id.value));
}

Result<void> GrantStore::record_revocation(const GrantId& id, const PublicKey& issuer, std::int64_t revoked_ms) const {
    const auto key = issuer.to_string();
    std::scoped_lock guard(impl_->lock);
    auto ok = execute(impl_->db, "INSERT OR IGNORE INTO grants_revocations (id, issuer, revoked_ms) VALUES (?1, ?2, ?3)",
                      {sv(id.value), sv(key), revoked_ms});
    if (!ok) return std::unexpected(ok.error());
    return {};
}

Result<std::vector<IssuedRecord>> GrantStore::issued() const {
    std::vector<IssuedRecord> out;
    std::scoped_lock guard(impl_->lock);
    auto ok = query(impl_->db, std::string(ISSUED) + " ORDER BY created_ms, id", {}, [&](const Row& r) -> Result<void> {
        auto row = issued_from_row(r);
        if (!row) return std::unexpected(row.error());
        IssuedRecord record{row->id, row->holder, std::nullopt, 0, false, std::nullopt, row->parent, row->created_ms, row->revoked_ms};
        if (auto* spec = std::get_if<GrantSpec>(&row->definition)) {
            record.scopes = std::move(spec->scopes);
            record.ttl_ms = spec->ttl_ms;
            record.reshare = spec->reshare;
            record.until_ms = spec->until_ms;
        } else {
            auto& n = std::get<Narrowing>(row->definition);
            record.scopes = std::move(n.scopes);
            record.ttl_ms = n.ttl_ms;
            record.reshare = n.reshare;
        }
        out.push_back(std::move(record));
        return {};
    });
    if (!ok) return std::unexpected(ok.error());
    return out;
}

Result<ReceivedGrant> GrantStore::receive(const PublicKey& own, const PublicKey& root,
                                          std::span<const std::uint8_t> token) const {
    return receive_checked(own, root, token, nullptr);
}

Result<ReceivedGrant> GrantStore::receive_renewal(const PublicKey& own, const PublicKey& root, const GrantId& expected,
                                                  std::span<const std::uint8_t> token) const {
    return receive_checked(own, root, token, &expected);
}

Result<ReceivedGrant> GrantStore::receive_checked(const PublicKey& own, const PublicKey& root,
                                                  std::span<const std::uint8_t> token, const GrantId* expected) const {
    auto chain = detail::Chain::parse(token, root);
    if (!chain) return std::unexpected(TrustError::malformed(chain.error().to_string()));
    const auto& last = chain->last();
    if (last.holder != own) return std::unexpected(TrustError::not_holder(last.grant.value));
    if (expected) {
        if (*expected != last.grant) return std::unexpected(TrustError::malformed("renewal returned a different grant"));
        auto old = received(*expected);
        if (!old) return std::unexpected(old.error());
        if (!*old) return std::unexpected(TrustError::unknown_grant(expected->value));
        if (chain->expires_ms() <= (*old)->expires_ms || last.issuer != (*old)->issuer)
            return std::unexpected(TrustError::malformed("renewal did not extend the original grant"));
    }
    ReceivedGrant grant{last.grant, last.issuer, root, std::vector<std::uint8_t>(token.begin(), token.end()),
                        chain->expires_ms(), last.reshare, std::nullopt};
    const auto issuer = grant.issuer.to_string();
    const auto root_hex = root.to_string();
    std::scoped_lock guard(impl_->lock);
    auto ok = execute(
        impl_->db,
        "INSERT INTO grants_received (id, issuer, root, token, expires_ms, reshare, refused_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)\n"
        "             ON CONFLICT(id) DO UPDATE SET token = excluded.token, expires_ms = excluded.expires_ms, reshare = excluded.reshare, refused_ms = NULL\n"
        "             WHERE excluded.issuer = grants_received.issuer AND excluded.root = grants_received.root",
        {sv(grant.id.value), sv(issuer), sv(root_hex), std::span<const std::uint8_t>(grant.token), grant.expires_ms,
         std::int64_t{grant.reshare ? 1 : 0}});
    if (!ok) return std::unexpected(ok.error());
    return grant;
}

Result<void> GrantStore::mark_refused(const GrantId& id, std::int64_t now_ms) const {
    std::scoped_lock guard(impl_->lock);
    auto ok = execute(impl_->db, "UPDATE grants_received SET refused_ms = ?2 WHERE id = ?1", {sv(id.value), now_ms});
    if (!ok) return std::unexpected(ok.error());
    return {};
}

Result<std::optional<ReceivedGrant>> GrantStore::received(const GrantId& id) const {
    std::optional<ReceivedGrant> out;
    std::scoped_lock guard(impl_->lock);
    auto ok = query(impl_->db, std::string(RECEIVED) + " WHERE id = ?1", {sv(id.value)}, [&](const Row& r) -> Result<void> {
        auto grant = received_from_row(r);
        if (!grant) return std::unexpected(grant.error());
        out = std::move(*grant);
        return {};
    });
    if (!ok) return std::unexpected(ok.error());
    return out;
}

Result<std::vector<ReceivedGrant>> GrantStore::received_all() const {
    std::vector<ReceivedGrant> out;
    std::scoped_lock guard(impl_->lock);
    auto ok = query(impl_->db, std::string(RECEIVED) + " ORDER BY expires_ms, id", {}, [&](const Row& r) -> Result<void> {
        auto grant = received_from_row(r);
        if (!grant) return std::unexpected(grant.error());
        out.push_back(std::move(*grant));
        return {};
    });
    if (!ok) return std::unexpected(ok.error());
    return out;
}

bool GrantStore::is_revoked(const GrantId& grant) const {
    bool revoked = true;
    std::scoped_lock guard(impl_->lock);
    auto ok = query(impl_->db, "SELECT EXISTS(SELECT 1 FROM grants_revocations WHERE id = ?1)", {sv(grant.value)},
                    [&](const Row& r) -> Result<void> {
                        revoked = r.integer(0) != 0;
                        return {};
                    });
    return ok ? revoked : true;
}

} // namespace loa::trust
