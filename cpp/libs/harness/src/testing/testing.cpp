#include "loa/harness/testing/testing.hpp"

#include <array>
#include <cstdlib>
#include <format>
#include <sstream>

#include "loa/core/xml.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

namespace loa::harness::testing {

namespace {

std::expected<std::string, std::string> read_text(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(path.string() + ": No such file or directory (os error 2)");
    return std::string(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

std::expected<void, loa::Error> write_text(const std::filesystem::path& path, std::string_view text) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    if (!out) return std::unexpected(loa::Error(ErrorCode::io, "cannot write " + path.string()));
    out << text;
    if (!out.flush()) return std::unexpected(loa::Error(ErrorCode::io, "cannot write " + path.string()));
    return {};
}

std::vector<std::string_view> lines(std::string_view text) {
    std::vector<std::string_view> out;
    while (!text.empty()) {
        auto nl = text.find('\n');
        std::string_view line = text.substr(0, nl);
        if (line.ends_with('\r')) line.remove_suffix(1);
        out.push_back(line);
        if (nl == std::string_view::npos) break;
        text.remove_prefix(nl + 1);
    }
    return out;
}

constexpr std::string_view SCRIPT = R"__(#!/usr/bin/env bash
dir='@DIR@'
if [[ "$1" == "--version" ]]; then cat "$dir/version"; exit 0; fi
printf '%s\n' "$@" > "$dir/args"
: > "$dir/stdin.log"
declare -A ids
exec 3< "$dir/script"
while IFS= read -r step <&3 || [[ -n "$step" ]]; do
  # Transcript fixtures may have CRLF when checked out on Windows.
  step="${step%$'\r'}"
  case "$step" in
    '' | '#'*) ;;
    '> '*)
      out="${step:2}"
      for k in "${!ids[@]}"; do out="${out//"{{$k}}"/${ids[$k]}}"; done
      printf '%s\n' "$out" ;;
    '< '*)
      spec="${step:2}"; want="${spec%% *}"; name=""
      if [[ "$spec" == *' '* ]]; then name="${spec#* }"; fi
      if ! IFS= read -r line; then exit 0; fi
      printf '%s\n' "$line" >> "$dir/stdin.log"
      if [[ "$line" != *"\"type\":\"$want\""* && "$line" != *"\"method\":\"$want\""* ]]; then
        echo "fake harness: expected $want, got $line" >&2; exit 3
      fi
      if [[ -n "$name" && "$line" =~ \"id\":\"?([^\",}]*) ]]; then ids[$name]="${BASH_REMATCH[1]}"; fi ;;
    'stderr '*) echo "${step:7}" >&2 ;;
    'exit '*) exit "${step:5}" ;;
  esac
done
while IFS= read -r line; do printf '%s\n' "$line" >> "$dir/stdin.log"; done
exit 0
)__";

} // namespace

std::expected<loa::core::Patch, loa::core::CommitError> WatchSink::commit_at(loa::core::Draft draft, std::int64_t ts) {
    auto patch = inner.commit_at(std::move(draft), ts);
    if (patch) {
        {
            std::scoped_lock lock(mutex_);
            seq_ = patch->seq;
        }
        changed_.notify_all();
    }
    return patch;
}

std::uint64_t WatchSink::seq() const {
    std::scoped_lock lock(mutex_);
    return seq_;
}

std::expected<void, std::string> WatchSink::until(std::string_view what, const std::function<bool(const loa::core::Dom&)>& done,
                                                  std::chrono::milliseconds timeout) const {
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    std::unique_lock lock(mutex_);
    for (;;) {
        const std::uint64_t seen = seq_;
        lock.unlock();
        if (inner.with_dom(done)) return {};
        lock.lock();
        if (!changed_.wait_until(lock, deadline, [&] { return seq_ != seen; })) break;
    }
    lock.unlock();
    if (inner.with_dom(done)) return {};
    return std::unexpected(std::format("timed out waiting for {}:\n{}", what, inner.with_dom([](const loa::core::Dom& d) { return loa::core::xml::to_string(d); })));
}

std::expected<void, std::string> golden(const std::filesystem::path& path, std::string_view actual) {
    if (std::getenv("LOA_BLESS") != nullptr) {
        if (auto written = write_text(path, actual); !written) return std::unexpected(written.error().message());
    }
    auto expected = read_text(path);
    if (!expected) return std::unexpected(expected.error());
    if (*expected != actual) return std::unexpected(path.string() + " differs (LOA_BLESS=1 rewrites it)");
    return {};
}

std::string render(const loa::core::Dom& dom) {
    const std::string xml = loa::core::xml::to_string(dom);
    static constexpr std::array<std::string_view, 3> names{" started.int=\"", " ended.int=\"", " created.int=\""};
    std::string out;
    const auto all = lines(xml);
    for (std::size_t i = 2; i < all.size(); ++i) {
        std::string_view rest = all[i];
        for (;;) {
            std::size_t at = std::string_view::npos;
            for (auto name : names)
                if (auto pos = rest.find(name); pos != std::string_view::npos) at = std::min(at, pos + name.size());
            if (at == std::string_view::npos) break;
            out.append(rest.substr(0, at));
            out.push_back('T');
            const auto quote = rest.find('"', at);
            rest.remove_prefix(at + (quote == std::string_view::npos ? 0 : quote - at));
        }
        out.append(rest);
        out.push_back('\n');
    }
    return out;
}

std::expected<void, std::string> round_trip(const loa::harness::kit::NativeCodec& codec, const std::filesystem::path& native) {
    auto import = [&](const std::filesystem::path& path) -> std::expected<loa::core::Dom, std::string> {
        loa::core::MemorySink sink;
        if (auto r = codec.import(path, sink); !r) return std::unexpected("importing " + path.string() + ": " + r.error().message());
        return sink.dom();
    };
    auto first = import(native);
    if (!first) return std::unexpected(first.error());
    auto dir = loa::runtime::TempDir::create();
    if (!dir) return std::unexpected(dir.error().to_string());
    const auto name = native.filename().empty() ? std::filesystem::path("session") : native.filename();
    const auto out = dir->path() / name;
    if (auto r = codec.export_(*first, out); !r) return std::unexpected("exporting " + native.string() + ": " + r.error().message());
    auto second = import(out);
    if (!second) return std::unexpected(second.error());
    if (loa::core::xml::to_string(*second) != loa::core::xml::to_string(*first)) return std::unexpected(native.string());
    return {};
}

std::expected<FakeProcess, loa::Error> FakeProcess::install(const std::filesystem::path& dir, std::string_view script) {
    const auto executable = dir / "harness";
    if (auto r = write_text(dir / "script", script); !r) return std::unexpected(r.error());
    if (auto r = write_text(dir / "version", "19.0.0\n"); !r) return std::unexpected(r.error());
    // Written by a separate process so no fork of this one can hold it open for writing when
    // it runs ("Text file busy"). Forward slashes: Git Bash on Windows reads them where
    // backslashes would need escaping, and every other platform is unaffected.
    std::string body(SCRIPT);
    std::string where = dir.string();
    std::ranges::replace(where, '\\', '/');
    body.replace(body.find("@DIR@"), 5, where);
    if (auto r = loa::runtime::exec::write_executable(executable, body); !r) return std::unexpected(r.error());
    // What to spawn: on Windows the `.cmd` launcher written beside the script.
    return FakeProcess(dir, loa::runtime::exec::resolve(executable));
}

std::expected<void, loa::Error> FakeProcess::set_version(std::string_view version) const {
    return write_text(dir_ / "version", std::string(version) + "\n");
}

std::vector<std::string> FakeProcess::args() const {
    auto text = read_text(dir_ / "args");
    if (!text) return {};
    std::vector<std::string> out;
    for (auto line : lines(*text)) out.emplace_back(line);
    return out;
}

std::vector<loa::runtime::json::Value> FakeProcess::received() const {
    auto text = read_text(dir_ / "stdin.log");
    if (!text) return {};
    std::vector<loa::runtime::json::Value> out;
    for (auto line : lines(*text))
        if (auto v = loa::runtime::json::parse(line)) out.push_back(std::move(*v));
    return out;
}

std::string handshake_v2() {
    return std::format("> {}\n< negotiate_protocol neg\n> {{\"type\":\"response\",\"id\":\"{{{{neg}}}}\",\"command\":\"negotiate_protocol\",\"success\":true,\"data\":{{\"protocolVersion\":2}}}}\n",
                       READY_V2);
}

} // namespace loa::harness::testing
