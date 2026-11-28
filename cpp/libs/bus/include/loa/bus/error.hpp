#pragma once

// Everything that can go wrong with a journal, its snapshots or the store.

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <string>
#include <variant>

#include "loa/core/dom.hpp"
#include "loa/runtime/error.hpp"

namespace loa::bus {

namespace journal_error {
struct Io {
    std::filesystem::path path;
    // The OS error, as `std::io::Error` displays it.
    std::string source;
    friend bool operator==(const Io&, const Io&) = default;
};
struct NotAJournal {
    std::filesystem::path path;
    friend bool operator==(const NotAJournal&, const NotAJournal&) = default;
};
struct UnsupportedVersion {
    std::filesystem::path path;
    std::uint32_t found = 0;
    std::uint32_t supported = 0;
    friend bool operator==(const UnsupportedVersion&, const UnsupportedVersion&) = default;
};
struct CorruptHeader {
    std::filesystem::path path;
    friend bool operator==(const CorruptHeader&, const CorruptHeader&) = default;
};
// A damaged record that is followed by more data, so it isn't a torn write.
struct Corrupt {
    std::filesystem::path path;
    std::uint64_t offset = 0;
    std::string reason;
    friend bool operator==(const Corrupt&, const Corrupt&) = default;
};
struct SequenceGap {
    std::uint64_t last = 0;
    std::uint64_t got = 0;
    friend bool operator==(const SequenceGap&, const SequenceGap&) = default;
};
struct OutOfRange {
    std::uint64_t seq = 0;
    std::uint64_t last = 0;
    friend bool operator==(const OutOfRange&, const OutOfRange&) = default;
};
struct TooLarge {
    std::size_t bytes = 0;
    friend bool operator==(const TooLarge&, const TooLarge&) = default;
};
struct DomMismatch {
    std::uint64_t dom = 0;
    std::uint64_t journal = 0;
    friend bool operator==(const DomMismatch&, const DomMismatch&) = default;
};
struct Apply {
    std::uint64_t seq = 0;
    core::ApplyError source;
    friend bool operator==(const Apply&, const Apply&) = default;
};
struct InvalidSessionId {
    std::string id;
    friend bool operator==(const InvalidSessionId&, const InvalidSessionId&) = default;
};
struct Exists {
    std::string id;
    friend bool operator==(const Exists&, const Exists&) = default;
};
struct NotFound {
    std::string id;
    friend bool operator==(const NotFound&, const NotFound&) = default;
};
struct Locked {
    std::filesystem::path path;
    friend bool operator==(const Locked&, const Locked&) = default;
};
// The journal writer was closed and released its journal.
struct Closed {
    friend bool operator==(const Closed&, const Closed&) = default;
};
// A protobuf decode error; displayed transparently.
struct Proto {
    loa::Error error;
    friend bool operator==(const Proto&, const Proto&) = default;
};
} // namespace journal_error

using JournalError =
    std::variant<journal_error::Io, journal_error::NotAJournal, journal_error::UnsupportedVersion,
                 journal_error::CorruptHeader, journal_error::Corrupt, journal_error::SequenceGap,
                 journal_error::OutOfRange, journal_error::TooLarge, journal_error::DomMismatch, journal_error::Apply,
                 journal_error::InvalidSessionId, journal_error::Exists, journal_error::NotFound,
                 journal_error::Locked, journal_error::Closed, journal_error::Proto>;

// The Rust `Display` text.
[[nodiscard]] std::string to_string(const JournalError& e);
[[nodiscard]] loa::Error to_error(const JournalError& e);

} // namespace loa::bus
