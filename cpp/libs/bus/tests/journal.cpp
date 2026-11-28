// The on-disk journal: format, recovery, snapshots, forks, the store and the
// journal writer (port of `crates/loa-bus/tests/journal.rs`).

#include <doctest/doctest.h>

#include <unistd.h>

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <functional>
#include <iterator>
#include <map>
#include <optional>
#include <string>
#include <vector>

#include "loa/bus/journal.hpp"
#include "loa/bus/store.hpp"
#include "loa/bus/testkit.hpp"
#include "loa/bus/writer.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/tempfile.hpp"

namespace fs = std::filesystem;
namespace core = loa::core;
namespace je = loa::bus::journal_error;
using loa::bus::Journal;
using loa::bus::JournalError;
using loa::bus::JournalOptions;
using loa::bus::Patches;
using core::Dom;
using core::Draft;
using core::NodeId;
using core::Patch;
using core::Value;

namespace {

using Bytes = std::vector<std::uint8_t>;

loa::runtime::TempDir tempdir() { return loa::runtime::TempDir::create().value(); }

template <class T>
T must(loa::bus::JournalResult<T> r) {
    if (!r) FAIL(loa::bus::to_string(r.error()));
    return std::move(*r);
}

void must(loa::bus::JournalResult<void> r) {
    if (!r) FAIL(loa::bus::to_string(r.error()));
}

JournalOptions every(std::uint64_t n) { return JournalOptions{n, 2}; }
JournalOptions no_snapshots() { return every(0); }

core::Attrs attrs(std::initializer_list<std::pair<std::string_view, Value>> pairs) {
    core::Attrs out;
    for (const auto& [k, v] : pairs) out.insert_or_assign(std::string(k), v);
    return out;
}

// `n` patches of every kind from an empty DOM, and the DOM after them.
std::pair<std::vector<Patch>, Dom> history(std::string_view id, std::size_t n) {
    namespace a = core::attr;
    Dom dom;
    std::vector<Patch> out;
    const auto commit = [&](Draft d) {
        auto p = dom.commit(std::move(d), 1'700'000'000'000 + static_cast<std::int64_t>(out.size()));
        REQUIRE(p);
        out.push_back(*p);
        return p->node;
    };
    const NodeId root = commit(Draft::insert(
        NodeId::DOCUMENT, core::ElementKind::Session,
        attrs({{a::ID, Value(id)}, {a::HARNESS, Value("omp")}, {a::NODE, Value("k")}, {a::CREATED, Value(1)}})));
    const NodeId turn = commit(Draft::insert(root, core::ElementKind::Turn));
    Value::Bytes all(256);
    for (int b = 0; b < 256; ++b) all[b] = static_cast<std::uint8_t>(b);
    commit(Draft::insert(root, core::ElementKind::Ext,
                         attrs({{a::HARNESS, Value("x")}, {"x-neg", Value(-42)}, {"x-raw", Value(all)}}),
                         "naïve \U0001F600"));
    for (std::int64_t i = 0;; ++i) {
        const auto ext = attrs({{a::HARNESS, Value("omp")},
                                {"x-f", Value(0.5)},
                                {"x-l", Value(Value::List{Value(true), Value(i)})}});
        const NodeId m = commit(Draft::insert(turn, core::ElementKind::Message, attrs({{a::ROLE, Value(core::Role::Assistant)}})));
        const NodeId e = commit(Draft::insert(root, core::ElementKind::Ext, ext, "raw"));
        switch (i % 4) {
        case 0: commit(Draft::append(m, "tok" + std::to_string(i) + " ")); break;
        case 1: commit(Draft::set(e, "x-n", Value(i))); break;
        case 2: commit(Draft::insert_at(root, 0, core::ElementKind::Ext, attrs({{a::HARNESS, Value("x")}}))); break;
        default: commit(Draft::remove(e)); break;
        }
        if (out.size() >= n) break;
    }
    out.resize(n);
    Dom replayed;
    for (const auto& p : out) REQUIRE(replayed.apply(p));
    return {out, replayed};
}

// The DOM right after `patches[..seq]`.
Dom dom_at(const std::vector<Patch>& patches, std::uint64_t seq) {
    Dom dom;
    for (std::uint64_t i = 0; i < seq; ++i) REQUIRE(dom.apply(patches[i]));
    return dom;
}

std::uint64_t file_len(const fs::path& p) { return fs::file_size(p); }
fs::path journal_file(const fs::path& dir) { return dir / loa::bus::JOURNAL_FILE; }

Bytes read_file(const fs::path& p) {
    std::ifstream in(p, std::ios::binary);
    REQUIRE(in);
    return Bytes(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

void write_file(const fs::path& p, const Bytes& bytes) {
    std::ofstream out(p, std::ios::binary | std::ios::trunc);
    out.write(reinterpret_cast<const char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    REQUIRE(out);
}

void write_file(const fs::path& p, std::string_view s) { write_file(p, Bytes(s.begin(), s.end())); }

void flip_byte(const fs::path& p, std::uint64_t offset) {
    auto bytes = read_file(p);
    bytes.at(offset) ^= 0x5a;
    write_file(p, bytes);
}

void set_len(const fs::path& p, std::uint64_t len) { REQUIRE(::truncate(p.c_str(), static_cast<off_t>(len)) == 0); }

// Snapshot files in `dir`, oldest first.
std::vector<fs::path> snapshot_files(const fs::path& dir) {
    std::vector<fs::path> files;
    for (const auto& e : fs::directory_iterator(dir))
        if (e.path().extension() == ".loas") files.push_back(e.path());
    std::sort(files.begin(), files.end());
    return files;
}

std::vector<Patch> collect(Patches patches) {
    std::vector<Patch> out;
    while (auto p = patches.next()) out.push_back(must(std::move(*p)));
    return out;
}

// Appends `patches` after the journal's last one as a writer would, syncs, and
// returns the offset of each appended record.
std::vector<std::uint64_t> write(Journal& journal, const std::vector<Patch>& patches, std::size_t upto = SIZE_MAX) {
    upto = std::min(upto, patches.size());
    Dom dom = dom_at(patches, journal.last_seq());
    const auto file = journal_file(journal.dir());
    std::vector<std::uint64_t> offsets;
    for (std::size_t i = journal.last_seq(); i < upto; ++i) {
        offsets.push_back(file_len(file));
        REQUIRE(dom.apply(patches[i]));
        must(journal.append_applied(patches[i], dom));
    }
    must(journal.sync());
    return offsets;
}

// A new journal `dir/id` holding `patches`, closed again.
std::pair<fs::path, std::vector<std::uint64_t>> written(const fs::path& dir, std::string_view id,
                                                        const std::vector<Patch>& patches, JournalOptions options) {
    const auto path = dir / std::string(id);
    auto journal = must(Journal::create(path, id, options));
    auto offsets = write(journal, patches);
    return {path, offsets};
}

template <class T>
bool is(const JournalError& e) {
    return std::holds_alternative<T>(e);
}

} // namespace

TEST_CASE("checked in journals open and rewrite byte for byte") {
    const fs::path fixtures = fs::path(LOA_GOLDEN_DIR) / "journal";
    const auto files = [](const fs::path& root) {
        std::map<fs::path, Bytes> out;
        for (const char* id : {"s", "f"})
            for (const auto& e : fs::directory_iterator(root / id))
                if (e.path().filename() != "lock") out[fs::relative(e.path(), root)] = read_file(e.path());
        return out;
    };
    auto opened = tempdir();
    for (const auto& [rel, bytes] : files(fixtures)) {
        fs::create_directories((opened.path() / rel).parent_path());
        write_file(opened.path() / rel, bytes);
    }
    std::vector<Patch> patches;
    {
        auto [s, cut] = must(Journal::open(opened.path() / "s", every(16)));
        patches = collect(must(s.patches(1)));
        CHECK(cut == std::nullopt);
        CHECK(s.last_seq() == 40);
        CHECK(s.snapshot_seqs() == std::vector<std::uint64_t>{16, 32});
        CHECK(must(s.load()) == dom_at(patches, 40));
        CHECK(must(s.load_at(20)) == dom_at(patches, 20));
        CHECK(must(Journal::read(opened.path() / "s")) == dom_at(patches, 40));
        // The snapshots decode to the DOMs at their sequences.
        auto metas = must(loa::bus::snapshot::list(opened.path() / "s"));
        REQUIRE(metas.size() == 2);
        for (const auto& m : metas) CHECK(must(loa::bus::snapshot::read_dom(m)) == dom_at(patches, m.seq));
        auto [f, fcut] = must(Journal::open(opened.path() / "f", every(16)));
        CHECK(f.parent() == loa::bus::Parent{"s", 0});
        CHECK(f.last_seq() == 0);
    }

    // Writing the same patches again reproduces every byte.
    auto again = tempdir();
    auto [dir, offsets] = written(again.path(), "s", patches, every(16));
    {
        auto [s, cut] = must(Journal::open(dir, every(16)));
        (void)must(s.fork(0, "f"));
    }
    CHECK(files(again.path()) == files(fixtures));
}

TEST_CASE("torn tails are cut and other damage is refused") {
    using Damage = std::function<std::uint64_t(const fs::path&, const std::vector<std::uint64_t>&, std::uint64_t)>;
    using Refused = std::function<bool(const JournalError&)>;
    struct Case {
        const char* name;
        Damage damage;
        Refused refused;
    };
    // Each gets the record offsets and the file length, damages the file and
    // returns its new length. Record 21 is the last.
    const Case cases[] = {
        {"half a record", [](auto& p, auto& o, auto e) { set_len(p, (o[20] + e) / 2); return (o[20] + e) / 2; }, {}},
        {"torn record header", [](auto& p, auto& o, auto) { set_len(p, o[20] + 3); return o[20] + 3; }, {}},
        {"zero filled tail",
         [](auto& p, auto& o, auto) {
             set_len(p, o[20]);
             set_len(p, o[20] + 4096);
             return o[20] + 4096;
         },
         {}},
        {"bad checksum on the last record", [](auto& p, auto& o, auto e) { flip_byte(p, (o[20] + e) / 2); return e; }, {}},
        {"damage before the last record", [](auto& p, auto& o, auto e) { flip_byte(p, o[9] + 10); return e; },
         is<je::Corrupt>},
        {"future version", [](auto& p, auto&, auto e) { flip_byte(p, 8); return e; }, is<je::UnsupportedVersion>},
        {"not a journal", [](auto& p, auto&, auto) { write_file(p, std::string_view("hello")); return std::uint64_t{5}; },
         is<je::NotAJournal>},
    };
    const auto [patches, _] = history("s", 21);
    for (const auto& c : cases) {
        CAPTURE(c.name);
        auto tmp = tempdir();
        auto [dir, offsets] = written(tmp.path(), "s", patches, no_snapshots());
        const auto path = journal_file(dir);
        const auto len = c.damage(path, offsets, file_len(path));
        auto opened = Journal::open(dir, no_snapshots());
        if (c.refused) {
            REQUIRE_FALSE(opened);
            CHECK(c.refused(opened.error()));
            continue;
        }
        REQUIRE(opened);
        auto& [journal, cut] = *opened;
        REQUIRE(cut);
        CHECK(cut->last_seq == 20);
        CHECK(cut->offset == offsets[20]);
        CHECK(cut->offset + cut->dropped_bytes == len);
        CHECK(file_len(path) == offsets[20]);
        CHECK(must(journal.load()) == dom_at(patches, 20));
        // Writing resumes at the cut and survives a reopen.
        must(journal.append(patches[20]));
        must(journal.sync());
        { auto closed = std::move(journal); }
        auto [reopened, cut2] = must(Journal::open(dir, no_snapshots()));
        CHECK(cut2 == std::nullopt);
        CHECK(must(reopened.load()) == dom_at(patches, 21));
    }
}

TEST_CASE("read only replay leaves a torn journal untouched") {
    auto tmp = tempdir();
    const auto [patches, _] = history("s", 21);
    const auto options = every(5);
    auto [dir, offsets] = written(tmp.path(), "s", patches, options);
    const auto path = journal_file(dir);
    set_len(path, offsets[20] + 3);
    const auto bytes = read_file(path);
    auto store = must(loa::bus::Store::make(tmp.path(), options));
    CHECK(must(store.read("s")) == dom_at(patches, 20));
    CHECK(read_file(path) == bytes);
}

TEST_CASE("appends must follow in order and reads start anywhere") {
    auto tmp = tempdir();
    const auto [patches, dom] = history("s", 1100);
    std::optional<Journal> journal = must(Journal::create(tmp.path() / "s", "s", every(700)));
    CHECK(journal->append(patches[1]).error() == JournalError(je::SequenceGap{0, 2}));
    CHECK(journal->append_applied(patches[0], Dom{}).error() == JournalError(je::DomMismatch{0, 1}));
    write(*journal, patches, 1099);
    CHECK(journal->append(patches[0]).error() == JournalError(je::SequenceGap{1099, 1}));
    auto before = must(journal->patches(1098));
    write(*journal, patches);
    CHECK(collect(std::move(before)).size() == 2);  // an iterator sees the journal as it was
    auto mismatch = journal->write_snapshot(dom_at(patches, 9));
    REQUIRE_FALSE(mismatch);
    REQUIRE(is<je::DomMismatch>(mismatch.error()));
    CHECK(std::get<je::DomMismatch>(mismatch.error()).dom == 9);
    for (bool reopen : {false, true}) {
        CAPTURE(reopen);
        if (reopen) {
            journal.reset();
            journal = must(Journal::open(tmp.path() / "s", every(700))).first;
        }
        for (std::uint64_t from : {0, 1, 700, 701, 1024, 1025, 1099, 1100, 1101}) {
            CAPTURE(from);
            const auto got = collect(must(journal->patches(from)));
            CHECK(got == std::vector<Patch>(patches.begin() + (std::max<std::uint64_t>(from, 1) - 1), patches.end()));
        }
        CHECK(journal->patches(1102).error() == JournalError(je::OutOfRange{1102, 1100}));
        CHECK(must(journal->load()) == dom);
        CHECK(must(journal->load_at(0)) == Dom{});
    }
}

TEST_CASE("damaged snapshots fall back to older ones then to a full replay") {
    using Damage = std::function<void(const fs::path&, const std::vector<fs::path>&)>;
    struct Case {
        const char* name;
        Damage damage;
        std::optional<std::vector<std::uint64_t>> seqs;
        bool loads;
    };
    // Record 10 is damaged in every case, so loading must not replay it.
    const Case cases[] = {
        {"intact", [](auto&, auto&) {}, std::vector<std::uint64_t>{100, 200}, true},
        {"newest body", [](auto&, auto& s) { flip_byte(s[1], file_len(s[1]) - 1); }, std::vector<std::uint64_t>{100, 200}, true},
        {"newest header", [](auto&, auto& s) { flip_byte(s[1], 13); }, std::vector<std::uint64_t>{100}, true},
        {"temp file from a crash",
         [](auto& d, auto&) { write_file(d / "snap-00000000000000000250.loas.tmp", std::string_view("LOASNAP\0", 8)); },
         std::vector<std::uint64_t>{100, 200}, true},
        {"both bodies", [](auto&, auto& s) { for (auto& f : s) flip_byte(f, file_len(f) - 1); },
         std::vector<std::uint64_t>{100, 200}, false},
        {"none left", [](auto&, auto& s) { for (auto& f : s) fs::remove(f); }, std::nullopt, false},
    };
    const auto [patches, dom] = history("s", 250);
    for (const auto& c : cases) {
        CAPTURE(c.name);
        auto tmp = tempdir();
        auto [dir, offsets] = written(tmp.path(), "s", patches, every(100));
        flip_byte(journal_file(dir), offsets[9] + 10);
        c.damage(dir, snapshot_files(dir));
        auto opened = Journal::open(dir, every(100));
        if (!opened) {
            CHECK(c.seqs == std::nullopt);
            continue;
        }
        auto& journal = opened->first;
        REQUIRE(c.seqs);
        CHECK(journal.snapshot_seqs() == *c.seqs);
        for (const auto& e : fs::directory_iterator(dir)) CHECK_FALSE(e.path().string().ends_with(".tmp"));
        auto loaded = journal.load();
        if (loaded) CHECK((c.loads && *loaded == dom));
        else CHECK((!c.loads && is<je::Corrupt>(loaded.error())));
        auto at = journal.load_at(230);
        CHECK(at.has_value() == c.loads);
        if (at && c.loads) CHECK(*at == dom_at(patches, 230));
    }
    // With an intact journal, damage to every snapshot ends in a full replay.
    auto tmp = tempdir();
    auto [dir, _] = written(tmp.path(), "s", patches, JournalOptions{60, 3});
    CHECK(snapshot_files(dir).size() == 3);  // older snapshots are pruned
    for (const auto& s : snapshot_files(dir)) flip_byte(s, file_len(s) - 1);
    CHECK(must(must(Journal::open(dir, every(100))).first.load()) == dom);
}

TEST_CASE("forks copy their history and link their parent") {
    auto tmp = tempdir();
    auto store = must(loa::bus::Store::make(tmp.path() / "sessions", every(16)));
    const auto [patches, dom] = history("src", 50);
    std::optional<Journal> src = must(store.create("src"));
    write(*src, patches);
    const auto contents = [](const fs::path& dir) {
        std::map<fs::path, Bytes> out;
        for (const auto& e : fs::directory_iterator(dir))
            if (e.path().filename() != "lock") out[e.path().filename()] = read_file(e.path());
        return out;
    };
    const auto before = contents(src->dir());

    std::optional<Journal> b = must(src->fork(20, "b"));
    CHECK(b->parent() == loa::bus::Parent{"src", 20});
    auto b_patches = collect(must(b->patches(1)));
    REQUIRE(b_patches.size() >= 20);
    CHECK(std::vector<Patch>(b_patches.begin(), b_patches.begin() + 20) ==
          std::vector<Patch>(patches.begin(), patches.begin() + 20));
    Dom b_dom = must(b->load());
    const auto root = [](const Dom& d, std::string_view name) -> std::optional<Value> {
        if (const auto* r = d.root())
            if (const auto* v = r->attr(name)) return *v;
        return std::nullopt;
    };
    CHECK(b->last_seq() == 23);  // the fork relabels its root
    CHECK(root(b_dom, core::attr::ID) == Value("b"));
    CHECK(root(b_dom, core::attr::PARENT_SESSION) == Value("src"));
    CHECK(root(b_dom, core::attr::FORKED_AT) == Value(20));
    std::optional<NodeId> ext;
    for (const auto& n : b_dom.iter())
        if (n.kind == core::ElementKind::Ext) {
            ext = n.id;
            break;
        }
    REQUIRE(ext);
    for (std::int64_t i = 0; i < 10; ++i) {
        auto p = b_dom.commit(Draft::set(*ext, "x-b", Value(i)), 9);
        REQUIRE(p);
        must(b->append_applied(*p, b_dom));
    }
    std::optional<Journal> c = must(b->fork(30, "c"));
    CHECK(c->parent()->session_id == "b");
    CHECK(must(c->load_at(30)) == must(b->load_at(30)));
    CHECK(must(c->load_at(20)) == dom_at(patches, 20));  // c carries the source's history too
    b.reset();
    CHECK(must(store.open("b")).first.load().value() == b_dom);

    CHECK(must(must(src->fork(0, "empty")).load()) == Dom{});
    CHECK(must(must(src->fork(50, "whole")).load_at(50)) == dom);
    CHECK(src->fork(51, "late").error() == JournalError(je::OutOfRange{51, 50}));
    CHECK(src->fork(10, "c").error() == JournalError(je::Exists{"c"}));
    CHECK_FALSE(fs::exists(store.root() / "late"));
    CHECK(contents(src->dir()) == before);  // forking never touches the source

    src.reset();
    c.reset();
    std::map<std::string, std::pair<std::optional<loa::bus::Parent>, std::uint64_t>> lineage;
    for (auto& l : must(store.list())) {
        if (auto* u = std::get_if<loa::bus::listing::Unreadable>(&l)) FAIL(u->id << ": " << loa::bus::to_string(u->error));
        auto& i = std::get<loa::bus::SessionInfo>(l);
        lineage[i.id] = {i.parent, i.last_seq};
    }
    CHECK(lineage["c"] == std::pair{std::optional(loa::bus::Parent{"b", 30}), std::uint64_t{33}});
    CHECK(lineage["src"] == std::pair{std::optional<loa::bus::Parent>(), std::uint64_t{50}});
    must(store.remove("src"));
    CHECK(is<je::NotFound>(store.open("src").error()));
    CHECK(must(store.open("c")).first.load_at(20).value() == dom_at(patches, 20));  // forks outlive their source
}

TEST_CASE("an open session is locked and bad ids and files are reported") {
    auto tmp = tempdir();
    auto store = must(loa::bus::Store::make(tmp.path() / "s", every(16)));
    for (const char* bad : {"", "..", "../x", "a/b", ".hidden", "a\\b", "a b"}) {
        CAPTURE(bad);
        CHECK(is<je::InvalidSessionId>(store.create(bad).error()));
    }
    std::optional<Journal> open = must(store.create("ok"));
    CHECK(is<je::Exists>(store.create("ok").error()));
    CHECK(is<je::Locked>(store.open("ok").error()));
    CHECK(is<je::Locked>(store.remove("ok").error()));
    fs::create_directory(store.root() / "broken");
    write_file(journal_file(store.root() / "broken"), std::string_view("not a journal"));
    auto listing = must(store.list());
    REQUIRE(listing.size() == 2);
    auto* broken = std::get_if<loa::bus::listing::Unreadable>(&listing[0]);
    REQUIRE(broken);
    CHECK(is<je::NotAJournal>(broken->error));
    REQUIRE(std::holds_alternative<loa::bus::SessionInfo>(listing[1]));
    CHECK(std::get<loa::bus::SessionInfo>(listing[1]).id == "ok");
    open.reset();
    (void)must(store.open("ok"));
    must(store.remove("ok"));
    CHECK(must(store.list()).size() == 1);
}

TEST_CASE("the journal writer syncs as its strategy says") {
    using loa::bus::SyncPolicy;
    auto tmp = tempdir();
    const auto [patches, dom] = history("w", 12);
    const auto hour = std::chrono::hours(1);
    std::vector<std::size_t> manual(12);
    for (std::size_t i = 0; i < 12; ++i) manual[i] = i + 1;
    const std::tuple<SyncPolicy, JournalOptions, std::vector<std::size_t>> cases[] = {
        // Synced at 4 and 8; the snapshot at 10 syncs too.
        {SyncPolicy::batch(4, hour), every(10), {1, 2, 3, 0, 1, 2, 3, 0, 1, 0, 1, 2}},
        {SyncPolicy::batch(UINT32_MAX, std::chrono::nanoseconds::zero()), no_snapshots(), std::vector<std::size_t>(12)},
        {SyncPolicy::every_patch(), no_snapshots(), std::vector<std::size_t>(12)},
        {SyncPolicy::manual(), no_snapshots(), manual},
    };
    int i = 0;
    for (const auto& [policy, options, expected] : cases) {
        const std::string id = "w" + std::to_string(i++);
        CAPTURE(id);
        loa::bus::JournalWriter writer(must(Journal::create(tmp.path() / id, id, options)), policy);
        Dom mirror;
        std::vector<std::size_t> pending;
        for (const auto& p : patches) {
            REQUIRE(mirror.apply(p));
            must(writer.append(p, mirror));
            pending.push_back(writer.pending());
        }
        CHECK(pending == expected);
        must(writer.sync());
        CHECK(writer.pending() == 0);
        must(writer.close());
        CHECK(is<je::Closed>(writer.append(patches[0], mirror).error()));
        CHECK(must(must(Journal::open(tmp.path() / id, no_snapshots())).first.load()) == dom);
    }
}

TEST_CASE("a session writer persists through the journal and snapshots from its own dom") {
    auto tmp = tempdir();
    auto store = must(loa::bus::Store::make(tmp.path(), JournalOptions{5, 2}));
    auto journal =
        std::make_shared<loa::bus::JournalWriter>(must(store.create("s1")), loa::bus::SyncPolicy::manual());
    loa::bus::NodeKey key;
    key.bytes.fill(7);
    auto writer = std::make_unique<loa::bus::SessionWriter>(loa::bus::WriterConfig{
        "s1", key, key, Dom{}, std::make_unique<loa::bus::JournalPersist>(journal),
        std::make_shared<loa::bus::testkit::DenyAll>()});
    const auto [patches, dom] = history("s1", 23);
    for (const auto& p : patches) {
        Draft draft = std::visit(
            [&](const auto& op) -> Draft {
                using T = std::decay_t<decltype(op)>;
                if constexpr (std::is_same_v<T, core::op::Insert>)
                    return Draft{Draft::Insert{op.parent, op.index, op.kind, op.attrs, op.text}};
                else if constexpr (std::is_same_v<T, core::op::Set>) return Draft{Draft::Set{p.node, op.name, op.value}};
                else if constexpr (std::is_same_v<T, core::op::Append>) return Draft{Draft::Append{p.node, op.text}};
                else return Draft{Draft::Remove{p.node}};
            },
            p.op);
        REQUIRE(writer->commit_at(std::move(draft), p.ts));
    }
    // Snapshots at 5, 10, 15 and 20, two kept, each syncing the journal.
    CHECK(journal->last_seq() == 23);
    CHECK(journal->pending() == 3);
    CHECK(snapshot_files(tmp.path() / "s1").size() == 2);
    CHECK(must(journal->load_at(12)) == dom_at(patches, 12));
    writer.reset();
    journal.reset();
    auto [reopened, cut] = must(store.open("s1"));
    CHECK(cut == std::nullopt);
    CHECK(reopened.snapshot_seqs() == std::vector<std::uint64_t>{15, 20});
    CHECK(must(reopened.load()) == dom);
}

TEST_CASE("payloads above the record limit are refused") {
    auto tmp = tempdir();
    auto journal = must(Journal::create(tmp.path() / "big", "big", no_snapshots()));
    const Patch big{1, NodeId{1}, 0,
                    core::op::Insert{NodeId::DOCUMENT, std::nullopt, core::ElementKind::Session, {},
                                     std::string(loa::bus::frame::max_payload + 1, 'x')}};
    const auto len = file_len(journal_file(journal.dir()));
    auto r = journal.append(big);
    REQUIRE_FALSE(r);
    REQUIRE(is<je::TooLarge>(r.error()));
    CHECK(std::get<je::TooLarge>(r.error()).bytes > loa::bus::frame::max_payload);
    CHECK(journal.last_seq() == 0);
    CHECK(file_len(journal_file(journal.dir())) == len);
}
