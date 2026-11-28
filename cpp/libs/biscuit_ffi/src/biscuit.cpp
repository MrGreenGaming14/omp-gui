#include "loa/biscuit_ffi/biscuit.hpp"

#include "loa/biscuit_ffi/loa_biscuit.h"

#include <algorithm>
#include <new>
#include <utility>

namespace loa::biscuit_ffi {

namespace detail {
void free_key_pair(LoaBscKeyPair* p) { loa_bsc_key_pair_free(p); }
void free_public_key(LoaBscPublicKey* p) { loa_bsc_public_key_free(p); }
void free_biscuit_builder(LoaBscBiscuitBuilder* p) { loa_bsc_biscuit_builder_free(p); }
void free_block_builder(LoaBscBlockBuilder* p) { loa_bsc_block_builder_free(p); }
void free_biscuit(LoaBscBiscuit* p) { loa_bsc_biscuit_free(p); }
void free_request(LoaBscThirdPartyRequest* p) { loa_bsc_third_party_request_free(p); }
void free_third_party_block(LoaBscThirdPartyBlock* p) { loa_bsc_third_party_block_free(p); }
void free_authorizer_builder(LoaBscAuthorizerBuilder* p) { loa_bsc_authorizer_builder_free(p); }
void free_authorizer(LoaBscAuthorizer* p) { loa_bsc_authorizer_free(p); }
}  // namespace detail

namespace {

using TermHandle = detail::Handle<LoaBscTerm, loa_bsc_term_free>;
using ParamsHandle = detail::Handle<LoaBscParams, loa_bsc_params_free>;
using FactsHandle = detail::Handle<LoaBscFacts, loa_bsc_facts_free>;

Error last_error() {
  Error e{static_cast<Status>(loa_bsc_error_status()), loa_bsc_error_message(), {}};
  const std::size_t n = loa_bsc_error_failed_check_count();
  e.checks.reserve(n);
  for (std::size_t i = 0; i < n; ++i) {
    FailedCheck c;
    const char* rule = nullptr;
    if (loa_bsc_error_failed_check(i, &c.is_block, &c.block_id, &c.check_id, &rule)) {
      c.rule = rule ? rule : "";
      e.checks.push_back(std::move(c));
    }
  }
  return e;
}

Result<void> check(LoaBscStatus s) {
  if (s == LOA_BSC_OK) return {};
  return std::unexpected(last_error());
}

template <class T>
T* require(T* p) {
  if (!p) throw std::bad_alloc();
  return p;
}

class Bytes {
 public:
  Bytes() = default;
  Bytes(const Bytes&) = delete;
  Bytes& operator=(const Bytes&) = delete;
  ~Bytes() { loa_bsc_bytes_free(b_); }
  LoaBscBytes* out() { return &b_; }
  bool present() const { return b_.data != nullptr; }
  std::span<const std::uint8_t> view() const { return {b_.data, b_.len}; }

 private:
  LoaBscBytes b_{nullptr, 0};
};

TermHandle to_ffi(const Term& t) {
  if (auto* s = std::get_if<std::string>(&t.value)) {
    // Interior NULs can't cross the C string boundary; Biscuit strings are UTF-8 text.
    return TermHandle(require(loa_bsc_term_string(s->c_str())));
  }
  if (auto* i = std::get_if<std::int64_t>(&t.value)) return TermHandle(require(loa_bsc_term_int(*i)));
  TermHandle set(require(loa_bsc_term_set()));
  for (const Term& element : std::get<std::vector<Term>>(t.value)) {
    TermHandle e = to_ffi(element);
    loa_bsc_term_set_insert(set.get(), e.get());
  }
  return set;
}

std::optional<Term> from_ffi(const LoaBscTerm* t) {
  switch (loa_bsc_term_kind(t)) {
    case LOA_BSC_TERM_STRING: {
      const char* data = nullptr;
      std::size_t len = 0;
      loa_bsc_term_as_string(t, &data, &len);
      return Term::string(std::string(data, len));
    }
    case LOA_BSC_TERM_INTEGER: {
      std::int64_t v = 0;
      loa_bsc_term_as_int(t, &v);
      return Term::integer(v);
    }
    case LOA_BSC_TERM_SET: {
      std::vector<Term> elements;
      const std::size_t n = loa_bsc_term_set_len(t);
      elements.reserve(n);
      for (std::size_t i = 0; i < n; ++i) {
        TermHandle e(loa_bsc_term_set_at(t, i));
        if (auto v = from_ffi(e.get())) elements.push_back(std::move(*v));
      }
      return Term::set(std::move(elements));
    }
    case LOA_BSC_TERM_OTHER:
      break;
  }
  return std::nullopt;
}

ParamsHandle to_ffi(const Params& params) {
  if (params.empty()) return nullptr;
  ParamsHandle p(require(loa_bsc_params_new()));
  for (const auto& [name, term] : params) {
    TermHandle t = to_ffi(term);
    loa_bsc_params_insert(p.get(), name.c_str(), t.get());
  }
  return p;
}

template <class B, LoaBscStatus (*Add)(B*, const char*, const LoaBscTerm* const*, std::size_t)>
Result<void> add_fact(B* builder, const Fact& fact) {
  std::vector<TermHandle> owned;
  std::vector<const LoaBscTerm*> terms;
  owned.reserve(fact.terms.size());
  terms.reserve(fact.terms.size());
  for (const Term& t : fact.terms) {
    owned.push_back(to_ffi(t));
    terms.push_back(owned.back().get());
  }
  return check(Add(builder, fact.name.c_str(), terms.data(), terms.size()));
}

template <class B, LoaBscStatus (*Code)(B*, const char*, const LoaBscParams*)>
Result<void> add_code(B* builder, std::string_view source, const Params& params) {
  ParamsHandle p = to_ffi(params);
  return check(Code(builder, std::string(source).c_str(), p.get()));
}

template <class T, class Raw>
Result<T> wrap(Raw* raw) {
  if (!raw) return std::unexpected(last_error());
  return T(raw);
}

}  // namespace

// ---- keys ----

Result<PublicKey> PublicKey::from_bytes(std::span<const std::uint8_t> bytes) {
  return wrap<PublicKey>(loa_bsc_public_key_from_bytes(bytes.data(), bytes.size()));
}

std::array<std::uint8_t, 32> PublicKey::bytes() const {
  std::array<std::uint8_t, 32> out{};
  loa_bsc_public_key_bytes(handle_.get(), out.data());
  return out;
}

Result<KeyPair> KeyPair::from_seed(std::span<const std::uint8_t> seed) {
  LoaBscKeyPair* raw = loa_bsc_key_pair_from_seed(seed.data(), seed.size());
  if (!raw) return std::unexpected(last_error());
  return KeyPair(raw);
}

PublicKey KeyPair::public_key() const { return PublicKey(require(loa_bsc_key_pair_public(handle_.get()))); }

Result<std::array<std::uint8_t, 64>> KeyPair::sign(std::span<const std::uint8_t> data) const {
  Bytes sig;
  if (auto r = check(loa_bsc_key_pair_sign(handle_.get(), data.data(), data.size(), sig.out())); !r) {
    return std::unexpected(r.error());
  }
  std::array<std::uint8_t, 64> out{};
  if (sig.view().size() != out.size()) return std::unexpected(Error{Status::internal, "unexpected signature size", {}});
  std::copy(sig.view().begin(), sig.view().end(), out.begin());
  return out;
}

// ---- builders ----

BlockBuilder::BlockBuilder() : handle_(require(loa_bsc_block_builder_new())) {}
Result<void> BlockBuilder::fact(const Fact& f) { return add_fact<LoaBscBlockBuilder, loa_bsc_block_builder_fact>(handle_.get(), f); }
Result<void> BlockBuilder::code(std::string_view source, const Params& params) {
  return add_code<LoaBscBlockBuilder, loa_bsc_block_builder_code>(handle_.get(), source, params);
}
Result<void> BlockBuilder::context(std::string_view context) {
  return check(loa_bsc_block_builder_context(handle_.get(), std::string(context).c_str()));
}

BiscuitBuilder::BiscuitBuilder() : handle_(require(loa_bsc_biscuit_builder_new())) {}
Result<void> BiscuitBuilder::fact(const Fact& f) { return add_fact<LoaBscBiscuitBuilder, loa_bsc_biscuit_builder_fact>(handle_.get(), f); }
Result<void> BiscuitBuilder::code(std::string_view source, const Params& params) {
  return add_code<LoaBscBiscuitBuilder, loa_bsc_biscuit_builder_code>(handle_.get(), source, params);
}
Result<void> BiscuitBuilder::context(std::string_view context) {
  return check(loa_bsc_biscuit_builder_context(handle_.get(), std::string(context).c_str()));
}
Result<Biscuit> BiscuitBuilder::build(const KeyPair& root) const {
  return wrap<Biscuit>(loa_bsc_biscuit_builder_build(handle_.get(), root.raw()));
}

// ---- token ----

Result<Biscuit> Biscuit::from(std::span<const std::uint8_t> token, const PublicKey& root) {
  return wrap<Biscuit>(loa_bsc_biscuit_from(token.data(), token.size(), root.raw()));
}

Result<std::vector<std::uint8_t>> Biscuit::to_vec() const {
  Bytes out;
  if (auto r = check(loa_bsc_biscuit_to_vec(handle_.get(), out.out())); !r) return std::unexpected(r.error());
  return std::vector<std::uint8_t>(out.view().begin(), out.view().end());
}

std::size_t Biscuit::block_count() const { return loa_bsc_biscuit_block_count(handle_.get()); }

std::vector<std::optional<std::string>> Biscuit::context() const {
  const std::size_t n = block_count();
  std::vector<std::optional<std::string>> out;
  out.reserve(n);
  for (std::size_t i = 0; i < n; ++i) {
    Bytes ctx;
    if (loa_bsc_biscuit_block_context(handle_.get(), i, ctx.out()) == LOA_BSC_OK && ctx.present()) {
      out.emplace_back(std::string(ctx.view().begin(), ctx.view().end()));
    } else {
      out.emplace_back(std::nullopt);
    }
  }
  return out;
}

std::vector<std::optional<PublicKey>> Biscuit::external_public_keys() const {
  const std::size_t n = block_count();
  std::vector<std::optional<PublicKey>> out;
  out.reserve(n);
  for (std::size_t i = 0; i < n; ++i) {
    if (LoaBscPublicKey* k = loa_bsc_biscuit_block_external_key(handle_.get(), i)) {
      out.emplace_back(PublicKey(k));
    } else {
      out.emplace_back(std::nullopt);
    }
  }
  return out;
}

Result<ThirdPartyRequest> Biscuit::third_party_request() const {
  return wrap<ThirdPartyRequest>(loa_bsc_biscuit_third_party_request(handle_.get()));
}

Result<Biscuit> Biscuit::append_third_party(const PublicKey& signer, const ThirdPartyBlock& block) const {
  return wrap<Biscuit>(loa_bsc_biscuit_append_third_party(handle_.get(), signer.raw(), block.raw()));
}

Result<ThirdPartyBlock> ThirdPartyRequest::create_block(const KeyPair& signer, const BlockBuilder& block) && {
  // The C call consumes the request whether or not it succeeds.
  return wrap<ThirdPartyBlock>(loa_bsc_third_party_request_create_block(handle_.release(), signer.raw(), block.raw()));
}

// ---- authorizer ----

AuthorizerBuilder::AuthorizerBuilder() : handle_(require(loa_bsc_authorizer_builder_new())) {}
Result<void> AuthorizerBuilder::code(std::string_view source, const Params& params) {
  return add_code<LoaBscAuthorizerBuilder, loa_bsc_authorizer_builder_code>(handle_.get(), source, params);
}
Result<void> AuthorizerBuilder::fact(const Fact& f) {
  return add_fact<LoaBscAuthorizerBuilder, loa_bsc_authorizer_builder_fact>(handle_.get(), f);
}
Result<void> AuthorizerBuilder::set_limits(const Limits& limits) {
  return check(loa_bsc_authorizer_builder_set_limits(handle_.get(), limits.max_facts, limits.max_iterations,
                                                     static_cast<std::uint64_t>(limits.max_time.count())));
}
Result<Authorizer> AuthorizerBuilder::build(const Biscuit& token) const {
  return wrap<Authorizer>(loa_bsc_authorizer_builder_build(handle_.get(), token.raw()));
}

Result<std::size_t> Authorizer::authorize() {
  std::size_t policy = 0;
  if (auto r = check(loa_bsc_authorizer_authorize(handle_.get(), &policy)); !r) return std::unexpected(r.error());
  return policy;
}

Result<std::vector<Fact>> Authorizer::query(std::string_view rule) {
  FactsHandle facts(loa_bsc_authorizer_query(handle_.get(), std::string(rule).c_str()));
  if (!facts) return std::unexpected(last_error());
  std::vector<Fact> out;
  const std::size_t n = loa_bsc_facts_len(facts.get());
  out.reserve(n);
  for (std::size_t i = 0; i < n; ++i) {
    Fact f;
    const char* name = nullptr;
    std::size_t len = 0;
    if (loa_bsc_facts_name(facts.get(), i, &name, &len)) f.name.assign(name, len);
    const std::size_t terms = loa_bsc_facts_term_count(facts.get(), i);
    for (std::size_t j = 0; j < terms; ++j) {
      auto t = from_ffi(loa_bsc_facts_term(facts.get(), i, j));
      if (!t) return std::unexpected(Error{Status::conversion, "unsupported term type in fact " + f.name, {}});
      f.terms.push_back(std::move(*t));
    }
    out.push_back(std::move(f));
  }
  return out;
}

}  // namespace loa::biscuit_ffi
