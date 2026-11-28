#include <doctest/doctest.h>

#include <array>
#include <expected>
#include <format>
#include <span>
#include <string>

namespace {

std::expected<int, std::string> parse_digit(char c) {
  if (c < '0' || c > '9') return std::unexpected(std::format("not a digit: {}", c));
  return c - '0';
}

int sum(std::span<const int> xs) {
  int total = 0;
  for (int x : xs) total += x;
  return total;
}

}  // namespace

TEST_CASE("std::expected carries value or error") {
  auto ok = parse_digit('7');
  REQUIRE(ok.has_value());
  CHECK(*ok == 7);
  auto bad = parse_digit('x');
  REQUIRE_FALSE(bad.has_value());
  CHECK(bad.error() == "not a digit: x");
  CHECK(parse_digit('3').transform([](int v) { return v * 2; }).value() == 6);
}

TEST_CASE("std::span views contiguous storage") {
  std::array<int, 4> a{1, 2, 3, 4};
  std::span<const int> s{a};
  CHECK(s.size() == 4);
  CHECK(sum(s) == 10);
  CHECK(sum(s.subspan(1, 2)) == 5);
}

TEST_CASE("std::format formats values") {
  CHECK(std::format("{}-{:03}", "loa", 7) == "loa-007");
  CHECK(std::format("{:.2f}", 1.5) == "1.50");
}
