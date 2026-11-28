// C++ wrapper over the loa biscuit-capi fork: the biscuit-auth 6.0 surface
// crates/loa-trust/src/token.rs uses. Tokens are byte-compatible with it.
#pragma once

#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

struct LoaBscKeyPair;
struct LoaBscPublicKey;
struct LoaBscBiscuitBuilder;
struct LoaBscBlockBuilder;
struct LoaBscBiscuit;
struct LoaBscThirdPartyRequest;
struct LoaBscThirdPartyBlock;
struct LoaBscAuthorizerBuilder;
struct LoaBscAuthorizer;

namespace loa::biscuit_ffi {

/// Biscuit error variants (mirrors `LoaBscStatus`).
enum class Status : int {
  ok = 0,
  invalid_argument = 1,
  internal = 2,
  format = 3,
  format_signature = 4,
  format_unknown_public_key = 5,
  append_on_sealed = 6,
  already_sealed = 7,
  no_matching_policy = 8,
  unauthorized = 9,
  invalid_block_rule = 10,
  authorizer_not_empty = 11,
  language = 12,
  too_many_facts = 13,
  too_many_iterations = 14,
  timeout = 15,
  unexpected_query_result = 16,
  conversion = 17,
  base64 = 18,
  execution = 19,
};

/// A check that failed during authorization. Authorizer checks have
/// `is_block == false` and `block_id == 0`.
struct FailedCheck {
  bool is_block = false;
  std::uint32_t block_id = 0;
  std::uint32_t check_id = 0;
  std::string rule;
};

struct Error {
  Status status = Status::internal;
  std::string message;
  /// Set for `no_matching_policy` and `unauthorized`.
  std::vector<FailedCheck> checks;
};

template <class T>
using Result = std::expected<T, Error>;

/// A Datalog term: string, integer or set (sets are kept in Biscuit order).
struct Term {
  std::variant<std::string, std::int64_t, std::vector<Term>> value;

  static Term string(std::string s) { return Term{std::move(s)}; }
  static Term integer(std::int64_t v) { return Term{v}; }
  static Term set(std::vector<Term> elements) { return Term{std::move(elements)}; }

  bool operator==(const Term&) const = default;
};

/// `{name}` parameters for Datalog source.
using Params = std::map<std::string, Term>;

struct Fact {
  std::string name;
  std::vector<Term> terms;
  bool operator==(const Fact&) const = default;
};

namespace detail {
template <class T, void (*Free)(T*)>
struct Deleter {
  void operator()(T* p) const noexcept { Free(p); }
};
template <class T, void (*Free)(T*)>
using Handle = std::unique_ptr<T, Deleter<T, Free>>;
void free_key_pair(LoaBscKeyPair*);
void free_public_key(LoaBscPublicKey*);
void free_biscuit_builder(LoaBscBiscuitBuilder*);
void free_block_builder(LoaBscBlockBuilder*);
void free_biscuit(LoaBscBiscuit*);
void free_request(LoaBscThirdPartyRequest*);
void free_third_party_block(LoaBscThirdPartyBlock*);
void free_authorizer_builder(LoaBscAuthorizerBuilder*);
void free_authorizer(LoaBscAuthorizer*);
}  // namespace detail

class PublicKey {
 public:
  static Result<PublicKey> from_bytes(std::span<const std::uint8_t> bytes);
  std::array<std::uint8_t, 32> bytes() const;
  bool operator==(const PublicKey& other) const { return bytes() == other.bytes(); }
  const LoaBscPublicKey* raw() const { return handle_.get(); }
  explicit PublicKey(LoaBscPublicKey* raw) : handle_(raw) {}

 private:
  detail::Handle<LoaBscPublicKey, detail::free_public_key> handle_;
};

/// Ed25519 key pair, the same key Biscuit signs with.
class KeyPair {
 public:
  /// From the 32-byte secret seed.
  static Result<KeyPair> from_seed(std::span<const std::uint8_t> seed);
  PublicKey public_key() const;
  Result<std::array<std::uint8_t, 64>> sign(std::span<const std::uint8_t> data) const;
  const LoaBscKeyPair* raw() const { return handle_.get(); }

 private:
  explicit KeyPair(LoaBscKeyPair* raw) : handle_(raw) {}
  detail::Handle<LoaBscKeyPair, detail::free_key_pair> handle_;
};

class Biscuit;

/// Attenuation block (signed by a third party via `ThirdPartyRequest`).
class BlockBuilder {
 public:
  BlockBuilder();
  Result<void> fact(const Fact& fact);
  Result<void> code(std::string_view source, const Params& params = {});
  Result<void> context(std::string_view context);
  const LoaBscBlockBuilder* raw() const { return handle_.get(); }

 private:
  detail::Handle<LoaBscBlockBuilder, detail::free_block_builder> handle_;
};

/// Authority block.
class BiscuitBuilder {
 public:
  BiscuitBuilder();
  Result<void> fact(const Fact& fact);
  Result<void> code(std::string_view source, const Params& params = {});
  Result<void> context(std::string_view context);
  Result<Biscuit> build(const KeyPair& root) const;

 private:
  detail::Handle<LoaBscBiscuitBuilder, detail::free_biscuit_builder> handle_;
};

class ThirdPartyBlock {
 public:
  const LoaBscThirdPartyBlock* raw() const { return handle_.get(); }
  explicit ThirdPartyBlock(LoaBscThirdPartyBlock* raw) : handle_(raw) {}

 private:
  detail::Handle<LoaBscThirdPartyBlock, detail::free_third_party_block> handle_;
};

class ThirdPartyRequest {
 public:
  /// Signs `block` with `signer`'s private key. Consumes the request.
  Result<ThirdPartyBlock> create_block(const KeyPair& signer, const BlockBuilder& block) &&;
  explicit ThirdPartyRequest(LoaBscThirdPartyRequest* raw) : handle_(raw) {}

 private:
  detail::Handle<LoaBscThirdPartyRequest, detail::free_request> handle_;
};

class Biscuit {
 public:
  /// Parses and verifies every signature against `root`.
  static Result<Biscuit> from(std::span<const std::uint8_t> token, const PublicKey& root);
  Result<std::vector<std::uint8_t>> to_vec() const;
  std::size_t block_count() const;
  /// Each block's context, authority first.
  std::vector<std::optional<std::string>> context() const;
  /// Each block's third-party signer, authority first.
  std::vector<std::optional<PublicKey>> external_public_keys() const;
  Result<ThirdPartyRequest> third_party_request() const;
  Result<Biscuit> append_third_party(const PublicKey& signer, const ThirdPartyBlock& block) const;
  const LoaBscBiscuit* raw() const { return handle_.get(); }
  explicit Biscuit(LoaBscBiscuit* raw) : handle_(raw) {}

 private:
  detail::Handle<LoaBscBiscuit, detail::free_biscuit> handle_;
};

/// Authorizer run limits; defaults are biscuit-auth's.
struct Limits {
  std::uint64_t max_facts = 1000;
  std::uint64_t max_iterations = 100;
  std::chrono::microseconds max_time{1000};
};

class Authorizer {
 public:
  /// Index of the matched allow policy.
  Result<std::size_t> authorize();
  /// Facts produced by `rule`, e.g. `r($a) <- right($a)`.
  Result<std::vector<Fact>> query(std::string_view rule);
  explicit Authorizer(LoaBscAuthorizer* raw) : handle_(raw) {}

 private:
  detail::Handle<LoaBscAuthorizer, detail::free_authorizer> handle_;
};

class AuthorizerBuilder {
 public:
  AuthorizerBuilder();
  Result<void> code(std::string_view source, const Params& params = {});
  Result<void> fact(const Fact& fact);
  Result<void> set_limits(const Limits& limits);
  Result<Authorizer> build(const Biscuit& token) const;

 private:
  detail::Handle<LoaBscAuthorizerBuilder, detail::free_authorizer_builder> handle_;
};

}  // namespace loa::biscuit_ffi
