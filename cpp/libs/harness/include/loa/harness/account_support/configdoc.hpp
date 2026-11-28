#pragma once

// Shared string-setting edits for harness configuration documents (port of
// `account_support/configdoc.rs`).

#include <initializer_list>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>

#include "loa/runtime/fs.hpp"
#include "loa/runtime/json.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::harness::account_support {

using loa::settings::models::Error;
using loa::settings::models::Result;

using KeyPath = std::span<const std::string_view>;

// A configuration document that string settings are read from and written to by key path.
class ConfigDoc {
  public:
    virtual ~ConfigDoc() = default;
    // The string at `path`; `nullopt` when missing or not a string.
    [[nodiscard]] virtual std::optional<std::string> get(KeyPath path) const = 0;
    // Sets the string at `path`, creating parents; `nullopt` removes it (missing parents
    // are left alone). A parent that is not an object/table is `Error::Kind::Unsupported`.
    [[nodiscard]] virtual Result<> set(KeyPath path, std::optional<std::string_view> value) = 0;
    [[nodiscard]] virtual std::string render() const = 0;

    [[nodiscard]] std::optional<std::string> get(std::initializer_list<std::string_view> path) const {
        return get(KeyPath(path.begin(), path.size()));
    }
    [[nodiscard]] Result<> set(std::initializer_list<std::string_view> path, std::optional<std::string_view> value) {
        return set(KeyPath(path.begin(), path.size()), value);
    }
};

// A JSON document: key order is kept and the rendering is two-space pretty JSON with a
// trailing newline (serde_json `to_string_pretty`).
class JsonDoc final : public ConfigDoc {
  public:
    // Empty text is an empty object; a root that is not an object is unsupported.
    [[nodiscard]] static Result<JsonDoc> parse(std::string_view text, std::string_view what);

    using ConfigDoc::get;
    using ConfigDoc::set;
    [[nodiscard]] std::optional<std::string> get(KeyPath path) const override;
    [[nodiscard]] Result<> set(KeyPath path, std::optional<std::string_view> value) override;
    [[nodiscard]] std::string render() const override;

    [[nodiscard]] const loa::runtime::json::Object& root() const noexcept { return root_; }

  private:
    JsonDoc(loa::runtime::json::Object root, std::string what) : root_(std::move(root)), what_(std::move(what)) {}
    loa::runtime::json::Object root_;
    std::string what_;
};

// A TOML document edited in place: untouched lines stay byte-identical, comments and the
// decoration around a replaced value are kept (the `toml_edit` behavior the Rust app has).
// Layouts the line editor can't reach (keys inside inline tables, tables defined only by
// dotted keys) are `Error::Kind::Unsupported`.
class TomlDoc final : public ConfigDoc {
  public:
    [[nodiscard]] static Result<TomlDoc> parse(std::string_view text, std::string_view what);

    TomlDoc(TomlDoc&&) noexcept;
    TomlDoc& operator=(TomlDoc&&) noexcept;
    ~TomlDoc() override;

    using ConfigDoc::get;
    using ConfigDoc::set;
    [[nodiscard]] std::optional<std::string> get(KeyPath path) const override;
    [[nodiscard]] Result<> set(KeyPath path, std::optional<std::string_view> value) override;
    [[nodiscard]] std::string render() const override { return text_; }

  private:
    struct Parsed;
    TomlDoc(std::string text, std::string what, std::unique_ptr<Parsed> parsed);
    std::string text_;
    std::string what_;
    std::unique_ptr<Parsed> parsed_;
};

// Parses `text` (the contents of `snapshot`'s file) as `D`, applies `edit` and saves the
// result through `snapshot` when it changed. Returns whether it changed.
template <typename D, typename F>
[[nodiscard]] Result<bool> edit_file(loa::runtime::fs::Snapshot& snapshot, std::string_view text, F&& edit) {
    const std::string what = snapshot.path().string();
    auto doc = D::parse(text, what);
    if (!doc) return std::unexpected(std::move(doc.error()));
    const std::string before = doc->render();
    if (Result<> edited = std::forward<F>(edit)(*doc); !edited) return std::unexpected(std::move(edited.error()));
    const std::string after = doc->render();
    if (before == after) return false;
    // Check the rendered document before committing it.
    if (auto check = D::parse(after, what); !check) return std::unexpected(std::move(check.error()));
    if (auto saved = loa::settings::models::save_snapshot(snapshot, after); !saved) return std::unexpected(std::move(saved.error()));
    return true;
}

} // namespace loa::harness::account_support
