#pragma once

// `omp/folder/1` (Rust `protocols::folder`): folders shared through folder
// grants. A read grant lists and reads, a write grant also writes; every path
// stays inside the folder. One request per stream: the request, the grant
// challenge, the owner's go-ahead, then a write's bytes or a read's chunks.

#include <cstddef>
#include <cstdint>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/p2p/protocols/host.hpp"

namespace loa::p2p::folder {

// Largest file read or written over the protocol.
inline constexpr std::uint64_t MAX_FILE = 16 * 1024 * 1024;
// File bytes per frame, well under the frame limit.
inline constexpr std::size_t CHUNK = 512 * 1024;

} // namespace loa::p2p::folder

namespace loa::p2p {

inline constexpr std::uint64_t MAX_FOLDER_FILE = folder::MAX_FILE;

// One entry of a shared folder.
struct FolderEntry {
    std::string name;
    bool dir = false;
    std::uint64_t size = 0;
    friend bool operator==(const FolderEntry&, const FolderEntry&) = default;
};

// A folder of a peer this node's grants reach.
struct SharedFolder {
    NodeId peer;
    // Absolute on the peer.
    std::string path;
    bool can_write = false;
    friend bool operator==(const SharedFolder&, const SharedFolder&) = default;
};

} // namespace loa::p2p

namespace loa::p2p::folder {

// The owner's side of one folder stream.
Result<> serve(NodeHost& host, const Connection& conn, Stream stream);

Result<std::vector<FolderEntry>> list_remote(NodeHost& host, const NodeId& peer, std::string_view folder,
                                             std::string_view path);
Result<Bytes> read_remote(NodeHost& host, const NodeId& peer, std::string_view folder, std::string_view path);
Result<> write_remote(NodeHost& host, const NodeId& peer, std::string_view folder, std::string_view path,
                      std::span<const std::uint8_t> bytes);

} // namespace loa::p2p::folder
