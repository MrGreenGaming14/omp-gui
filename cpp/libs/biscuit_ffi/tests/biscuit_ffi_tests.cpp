#include <doctest/doctest.h>

#include <loa/biscuit_ffi/biscuit.hpp>

#include <array>
#include <cstdint>
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

using namespace loa::biscuit_ffi;

namespace {

constexpr std::int64_t kNow = 1'800'000'000'000;

KeyPair key(std::uint8_t n) {
  std::array<std::uint8_t, 32> seed{};
  seed.fill(n);
  auto pair = KeyPair::from_seed(seed);
  REQUIRE(pair.has_value());
  return std::move(*pair);
}

std::string hex(const PublicKey& key) {
  static constexpr char digits[] = "0123456789abcdef";
  std::string out;
  for (auto b : key.bytes()) {
    out += digits[b >> 4];
    out += digits[b & 0xF];
  }
  return out;
}

std::vector<std::uint8_t> read_fixture() {
  std::ifstream in(std::string(LOA_BISCUIT_FIXTURE_DIR) + "/v1_reshared_token.bin", std::ios::binary);
  REQUIRE(in.good());
  return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

std::string context_json(const std::string& grant, const PublicKey& holder, std::int64_t expires, bool reshare) {
  return R"({"g":")" + grant + R"(","h":")" + hex(holder) + R"(","e":)" + std::to_string(expires) +
         R"(,"r":)" + (reshare ? "true" : "false") + "}";
}

constexpr std::string_view kExpiryCheck = "check if time_ms($t), $t < {expires};";

Biscuit mint(const KeyPair& issuer, const PublicKey& holder, std::int64_t expires) {
  BiscuitBuilder builder;
  REQUIRE(builder.fact({"right", {Term::string("one"), Term::string("view")}}).has_value());
  REQUIRE(builder.fact({"right", {Term::string("two"), Term::string("drive")}}).has_value());
  REQUIRE(builder.code(kExpiryCheck, {{"expires", Term::integer(expires)}}).has_value());
  REQUIRE(builder.context(context_json("g0", holder, expires, true)).has_value());
  auto token = builder.build(issuer);
  REQUIRE(token.has_value());
  return std::move(*token);
}

Biscuit attenuate(const Biscuit& parent, const KeyPair& holder, const PublicKey& new_holder, std::int64_t expires) {
  BlockBuilder block;
  REQUIRE(block.code(kExpiryCheck, {{"expires", Term::integer(expires)}}).has_value());
  REQUIRE(block.context(context_json("g1", new_holder, expires, false)).has_value());
  auto request = parent.third_party_request();
  REQUIRE(request.has_value());
  auto signed_block = std::move(*request).create_block(holder, block);
  REQUIRE(signed_block.has_value());
  auto token = parent.append_third_party(holder.public_key(), *signed_block);
  REQUIRE(token.has_value());
  return std::move(*token);
}

Result<std::size_t> authorize_at(const Biscuit& token, std::int64_t now, const Limits& limits = {}) {
  AuthorizerBuilder builder;
  REQUIRE(builder.code("allow if true;").has_value());
  REQUIRE(builder.fact({"time_ms", {Term::integer(now)}}).has_value());
  REQUIRE(builder.set_limits(limits).has_value());
  auto authorizer = builder.build(token);
  if (!authorizer) return std::unexpected(authorizer.error());
  return authorizer->authorize();
}

// Limit errors may surface when the authorizer is built or when it runs.
Result<std::size_t> run_with(const Biscuit& token, std::string_view code, const Limits& limits) {
  AuthorizerBuilder builder;
  REQUIRE(builder.code(code).has_value());
  REQUIRE(builder.set_limits(limits).has_value());
  auto authorizer = builder.build(token);
  if (!authorizer) return std::unexpected(authorizer.error());
  return authorizer->authorize();
}

}  // namespace

TEST_CASE("v1 fixture parses with its context and signers") {
  auto bytes = read_fixture();
  auto token = Biscuit::from(bytes, key(1).public_key());
  REQUIRE(token.has_value());
  CHECK(token->block_count() == 2);

  auto contexts = token->context();
  REQUIRE(contexts.size() == 2);
  const std::string holders[] = {hex(key(2).public_key()), hex(key(3).public_key())};
  for (std::size_t i = 0; i < 2; ++i) {
    REQUIRE(contexts[i].has_value());
    const std::string& c = *contexts[i];
    CHECK(c.front() == '{');
    CHECK(c.back() == '}');
    auto g = c.find(R"("g":)");
    auto h = c.find(R"("h":")" + holders[i] + '"');
    auto e = c.find(R"("e":)");
    auto r = c.find(R"("r":)");
    CHECK(g == 1);
    REQUIRE(h != std::string::npos);
    REQUIRE(e != std::string::npos);
    REQUIRE(r != std::string::npos);
    CHECK(g < h);
    CHECK(h < e);
    CHECK(e < r);
    CHECK(std::stoll(c.substr(e + 4)) > kNow);
  }

  auto signers = token->external_public_keys();
  REQUIRE(signers.size() == 2);
  CHECK_FALSE(signers[0].has_value());
  REQUIRE(signers[1].has_value());
  CHECK(*signers[1] == key(2).public_key());
}

TEST_CASE("fixture rejects the wrong root") {
  auto bytes = read_fixture();
  CHECK_FALSE(Biscuit::from(bytes, key(9).public_key()).has_value());
}

TEST_CASE("fixture round-trips byte for byte") {
  auto bytes = read_fixture();
  auto token = Biscuit::from(bytes, key(1).public_key());
  REQUIRE(token.has_value());
  auto out = token->to_vec();
  REQUIRE(out.has_value());
  CHECK(*out == bytes);
}

TEST_CASE("mint and third-party attenuate round-trip") {
  constexpr std::int64_t expires = 5'800'000'000'000;
  auto root = key(1);
  auto token = attenuate(mint(root, key(2).public_key(), expires), key(2), key(3).public_key(), expires);
  auto bytes = token.to_vec();
  REQUIRE(bytes.has_value());
  if (const char* path = std::getenv("LOA_BISCUIT_WRITE_TOKEN")) {
    std::ofstream(path, std::ios::binary).write(reinterpret_cast<const char*>(bytes->data()),
                                                static_cast<std::streamsize>(bytes->size()));
  }

  auto parsed = Biscuit::from(*bytes, root.public_key());
  REQUIRE(parsed.has_value());
  CHECK(parsed->block_count() == 2);
  auto contexts = parsed->context();
  REQUIRE(contexts.size() == 2);
  CHECK(contexts[0] == context_json("g0", key(2).public_key(), expires, true));
  CHECK(contexts[1] == context_json("g1", key(3).public_key(), expires, false));
  auto signers = parsed->external_public_keys();
  REQUIRE(signers.size() == 2);
  CHECK_FALSE(signers[0].has_value());
  REQUIRE(signers[1].has_value());
  CHECK(*signers[1] == key(2).public_key());
}

TEST_CASE("expiry checks") {
  constexpr std::int64_t expires = kNow + 1000;
  auto token = mint(key(1), key(2).public_key(), expires);
  CHECK(authorize_at(token, expires - 1).has_value());
  for (auto now : {expires, expires + 1}) {
    auto denied = authorize_at(token, now);
    REQUIRE_FALSE(denied.has_value());
    CHECK(denied.error().status == Status::unauthorized);
    CHECK_FALSE(denied.error().checks.empty());
  }

  // Re-share block expires earlier than the authority block.
  auto narrowed = attenuate(token, key(2), key(3).public_key(), expires - 500);
  auto denied = authorize_at(narrowed, expires - 100);
  REQUIRE_FALSE(denied.has_value());
  CHECK(denied.error().status == Status::unauthorized);
  bool block_one = false;
  for (const auto& check : denied.error().checks) block_one |= check.is_block && check.block_id == 1;
  CHECK(block_one);
}

TEST_CASE("authorizer limits") {
  auto token = mint(key(1), key(2).public_key(), kNow);
  const Limits limits{1000, 100, std::chrono::milliseconds(250)};

  SUBCASE("too many facts") {
    std::string code;
    for (int i = 0; i < 10; ++i) code += "n(" + std::to_string(i) + ");\n";
    code += "p($x, $y, $z) <- n($x), n($y), n($z);\nallow if true;";
    auto result = run_with(token, code, limits);
    REQUIRE_FALSE(result.has_value());
    CHECK(result.error().status == Status::too_many_facts);
  }
  // Datalog heads only bind variables from body predicates, so workloads walk
  // a chain of literal `edge` facts.
  auto chain = [](int length, std::string_view rules) {
    std::string code;
    for (int i = 0; i < length; ++i) code += "edge(" + std::to_string(i) + ", " + std::to_string(i + 1) + ");\n";
    code += rules;
    return code + "\nallow if true;";
  };
  SUBCASE("too many iterations") {
    // One new reach fact per iteration: 200 iterations, ~400 facts.
    auto result = run_with(token, chain(200, "reach(0);\nreach($y) <- reach($x), edge($x, $y);"), limits);
    REQUIRE_FALSE(result.has_value());
    CHECK(result.error().status == Status::too_many_iterations);
  }
  SUBCASE("timeout") {
    // Transitive closure of a long chain: quadratic facts, linear iterations.
    const Limits loose{1'000'000'000, 1'000'000'000, std::chrono::milliseconds(250)};
    auto result = run_with(token, chain(3000, "path($x, $y) <- edge($x, $y);\npath($x, $z) <- path($x, $y), edge($y, $z);"), loose);
    REQUIRE_FALSE(result.has_value());
    CHECK(result.error().status == Status::timeout);
  }
}

TEST_CASE("query and set parameters") {
  auto token = mint(key(1), key(2).public_key(), kNow + 1000);
  AuthorizerBuilder builder;
  REQUIRE(builder.code("allow if true;").has_value());
  REQUIRE(builder.fact({"time_ms", {Term::integer(kNow)}}).has_value());
  REQUIRE(builder.code("check if {s}.contains(\"a\");", {{"s", Term::set({Term::string("a"), Term::string("b")})}})
              .has_value());
  auto authorizer = builder.build(token);
  REQUIRE(authorizer.has_value());
  CHECK(authorizer->authorize().has_value());

  auto facts = authorizer->query("r($a) <- right($x, $a)");
  REQUIRE(facts.has_value());
  REQUIRE(facts->size() == 2);
  auto has = [&](const char* a) {
    for (const auto& f : *facts)
      if (f == Fact{"r", {Term::string(a)}}) return true;
    return false;
  };
  CHECK(has("view"));
  CHECK(has("drive"));
}

TEST_CASE("keys sign and round-trip") {
  auto pair = key(7);
  const std::uint8_t a[] = {'d', 'i', 'a', 'l'};
  const std::uint8_t b[] = {'d', 'i', 'a', 'm'};
  auto sa = pair.sign(a);
  auto sb = pair.sign(b);
  REQUIRE(sa.has_value());
  REQUIRE(sb.has_value());
  CHECK(sa->size() == 64);
  CHECK(*sa != *sb);

  auto pub = pair.public_key();
  auto bytes = pub.bytes();
  auto back = PublicKey::from_bytes(bytes);
  REQUIRE(back.has_value());
  CHECK(*back == pub);

  std::array<std::uint8_t, 31> short_seed{};
  auto bad = KeyPair::from_seed(short_seed);
  REQUIRE_FALSE(bad.has_value());
  CHECK(bad.error().status == Status::invalid_argument);
}
