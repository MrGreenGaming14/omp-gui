#pragma once

// The projects library's error: one kind per Rust `loa_projects::Error` variant, displayed like it.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <string>

#include "loa/runtime/error.hpp"

namespace loa::projects {

enum class ErrorKind : std::uint8_t {
    // A port (session source, settings store, writer) failed.
    Port,
    // Missing, in another project or denied: not told apart, so access isn't revealed.
    SessionNotFound,
    NodeNotFound,
    Summarizer,
    InvalidAnswer,
    NothingToDigest,
    Arguments,
    EmptyName,
    ProjectExists,
    // A file the user also edits outside Loa changed after it was loaded; nothing was written.
    ChangedOnDisk,
    Commit,
    Io,
};

struct Error {
    ErrorKind kind = ErrorKind::Port;
    // The variant's payload: port/summarizer/answer/argument/commit/io message, session id or project name.
    std::string detail;
    // NodeNotFound only.
    std::uint64_t node = 0;
    // ChangedOnDisk only.
    std::filesystem::path path;

    [[nodiscard]] static Error port(std::string message) { return {ErrorKind::Port, std::move(message)}; }
    [[nodiscard]] static Error session_not_found(std::string session) { return {ErrorKind::SessionNotFound, std::move(session)}; }
    [[nodiscard]] static Error node_not_found(std::string session, std::uint64_t node) {
        return {ErrorKind::NodeNotFound, std::move(session), node};
    }
    [[nodiscard]] static Error summarizer(std::string message) { return {ErrorKind::Summarizer, std::move(message)}; }
    [[nodiscard]] static Error invalid_answer(std::string why) { return {ErrorKind::InvalidAnswer, std::move(why)}; }
    [[nodiscard]] static Error nothing_to_digest() { return {ErrorKind::NothingToDigest, {}}; }
    [[nodiscard]] static Error arguments(std::string why) { return {ErrorKind::Arguments, std::move(why)}; }
    [[nodiscard]] static Error empty_name() { return {ErrorKind::EmptyName, {}}; }
    [[nodiscard]] static Error project_exists(std::string name) { return {ErrorKind::ProjectExists, std::move(name)}; }
    [[nodiscard]] static Error changed_on_disk(std::filesystem::path path) {
        return {ErrorKind::ChangedOnDisk, {}, 0, std::move(path)};
    }
    [[nodiscard]] static Error commit(std::string message) { return {ErrorKind::Commit, std::move(message)}; }
    [[nodiscard]] static Error io(std::string message) { return {ErrorKind::Io, std::move(message)}; }

    // The Rust `Display` text.
    [[nodiscard]] std::string to_string() const;
    [[nodiscard]] loa::Error to_error() const;
    friend bool operator==(const Error&, const Error&) = default;
};

template <class T = void>
using Result = std::expected<T, Error>;

} // namespace loa::projects
