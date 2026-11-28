#include "loa/sandbox/isolation.hpp"

#include <unistd.h>

#include <atomic>
#include <format>
#include <system_error>

#include "loa/runtime/fs.hpp"
#include "loa/runtime/time.hpp"
#include "process.hpp"

namespace loa::sandbox {

namespace fs = std::filesystem;

namespace {

std::atomic<std::uint64_t> NEXT{0};

Result<> io(const std::error_code& ec, std::string_view what) {
    if (!ec) return {};
    return std::unexpected(error::os(what, ec.value()));
}

detail::Spawn git(const fs::path& dir, std::vector<std::string> args) {
    detail::Spawn spec;
    spec.program = "git";
    spec.args = std::move(args);
    spec.cwd = dir;
    return spec;
}

// Runs `spec` and returns its stdout, or fails with its stderr.
Result<std::string> output(const detail::Spawn& spec, std::string_view input = {}) {
    auto out = detail::run(spec, input);
    if (!out) return std::unexpected(out.error());
    if (!out->success()) {
        std::string args;
        for (const auto& a : spec.args) args += (args.empty() ? "" : " ") + a;
        return std::unexpected(error::isolation(std::format("{} {}: {}", spec.program.string(), args, detail::lossy_trim(out->err))));
    }
    return std::move(out->out);
}

Result<fs::path> canonical_path(const fs::path& path) { return runtime::fs::canonicalize(path); }

Result<fs::path> toplevel(const fs::path& dir) {
    auto out = output(git(dir, {"rev-parse", "--show-toplevel"}));
    if (!out) return std::unexpected(out.error());
    while (!out->empty() && (out->back() == '\n' || out->back() == '\r' || out->back() == ' ' || out->back() == '\t')) out->pop_back();
    return canonical_path(*out);
}

const GitWorktree WORKTREE;
const GitClone CLONE;

// The backends to try, in order.
std::vector<const CheckoutBackend*> backends() { return {&WORKTREE, &CLONE}; }

// Replaces `dst` with a copy of `src` (a file, symlink or directory tree).
Result<> copy_entry(const fs::path& src, const fs::path& dst) {
    std::error_code ec;
    const auto status = fs::symlink_status(src, ec);
    if (auto r = io(ec, src.string()); !r) return r;
    if (auto r = detail::remove_entry(dst); !r) return r;
    if (dst.has_parent_path()) {
        fs::create_directories(dst.parent_path(), ec);
        if (auto r = io(ec, dst.parent_path().string()); !r) return r;
    }
    if (fs::is_symlink(status)) {
        const auto target = fs::read_symlink(src, ec);
        if (auto r = io(ec, src.string()); !r) return r;
        fs::create_symlink(target, dst, ec);
        return io(ec, dst.string());
    }
    if (fs::is_directory(status)) {
        fs::create_directory(dst, ec);
        if (auto r = io(ec, dst.string()); !r) return r;
        for (const auto& entry : fs::directory_iterator(src, ec)) {
            if (auto r = copy_entry(entry.path(), dst / entry.path().filename()); !r) return r;
        }
        return io(ec, src.string());
    }
    if (fs::is_regular_file(status)) {
        fs::copy_file(src, dst, fs::copy_options::overwrite_existing, ec);
        if (auto r = io(ec, dst.string()); !r) return r;
        fs::permissions(dst, status.permissions(), ec);
        return io(ec, dst.string());
    }
    return std::unexpected(error::isolation("unsupported workspace entry: " + src.string()));
}

// Overlays the tracked and untracked source files onto the clean checkout; files Git lists but
// the source deleted are removed from it.
Result<> overlay(const fs::path& source, const fs::path& checkout) {
    auto files = output(git(source, {"ls-files", "--cached", "--others", "--exclude-standard", "-z"}));
    if (!files) return std::unexpected(files.error());
    std::string_view rest = *files;
    while (!rest.empty()) {
        const auto end = rest.find('\0');
        const auto path = rest.substr(0, end);
        rest.remove_prefix(end == std::string_view::npos ? rest.size() : end + 1);
        if (path.empty()) continue;
        const auto from = source / path, to = checkout / path;
        std::error_code ec;
        const bool exists = fs::exists(fs::symlink_status(from, ec));
        if (auto r = exists ? copy_entry(from, to) : detail::remove_entry(to); !r) return r;
    }
    return {};
}

} // namespace

namespace detail {
Result<> remove_entry(const fs::path& path) {
    std::error_code ec;
    const auto status = fs::symlink_status(path, ec);
    if (ec || !fs::exists(status)) return {};
    if (fs::is_directory(status)) fs::remove_all(path, ec);
    else fs::remove(path, ec);
    return io(ec, path.string());
}
} // namespace detail

Result<fs::path> GitWorktree::create(const fs::path& repo, std::string_view name) const {
    auto dst = fs::temp_directory_path() / name;
    auto out = output(git(repo, {"worktree", "add", "--detach", "--", dst.string(), "HEAD"}));
    if (!out) return std::unexpected(out.error());
    return dst;
}

Result<> GitWorktree::remove(const fs::path& repo, std::string_view, const fs::path& checkout) const {
    auto out = output(git(repo, {"worktree", "remove", "--force", "--", checkout.string()}));
    if (!out) return std::unexpected(out.error());
    return {};
}

Result<fs::path> GitClone::create(const fs::path& repo, std::string_view name) const {
    auto dst = fs::temp_directory_path() / name;
    auto out = output(git(fs::temp_directory_path(), {"clone", "--no-hardlinks", "--", repo.string(), dst.string()}));
    if (!out) return std::unexpected(out.error());
    return dst;
}

Result<> GitClone::remove(const fs::path&, std::string_view, const fs::path& checkout) const {
    std::error_code ec;
    fs::remove_all(checkout, ec);
    return io(ec, checkout.string());
}

std::string_view backend_name() { return backends().front()->name(); }

IsolatedCheckout::IsolatedCheckout(IsolatedCheckout&& other) noexcept
    : repo_(std::move(other.repo_)), name_(std::move(other.name_)), root_(std::move(other.root_)),
      path_(std::move(other.path_)), baseline_(std::move(other.baseline_)), backend_(std::exchange(other.backend_, nullptr)) {}

IsolatedCheckout::~IsolatedCheckout() { (void)remove(); }

Result<IsolatedCheckout> IsolatedCheckout::create(const fs::path& workspace) {
    auto source = canonical_path(workspace);
    if (!source) return std::unexpected(source.error());
    auto repo = toplevel(*source);
    if (!repo) return std::unexpected(repo.error());
    const auto relative = source->lexically_relative(*repo);
    if (relative.empty() || *relative.begin() == "..") {
        return std::unexpected(error::isolation("prefix not found"));
    }
    const auto name = std::format("loa-sandbox-{}-{:x}-{}", ::getpid(), runtime::time::now_ms(), NEXT.fetch_add(1));
    loa::Error failure = error::isolation("no checkout backend");
    for (const auto* backend : backends()) {
        auto root = backend->create(*repo, name);
        if (!root) {
            failure = root.error();
            continue;
        }
        IsolatedCheckout checkout;
        checkout.path_ = relative == "." ? *root : *root / relative;
        checkout.root_ = *root;
        checkout.repo_ = *repo;
        checkout.name_ = name;
        checkout.backend_ = backend;
        if (auto r = overlay(checkout.repo_, checkout.root_); !r) return std::unexpected(r.error());
        auto tree = checkout.stage({"write-tree"});
        if (!tree) return std::unexpected(tree.error());
        checkout.baseline_ = detail::lossy_trim(*tree);
        return checkout;
    }
    return std::unexpected(failure);
}

Result<std::string> IsolatedCheckout::stage(const std::vector<std::string>& args) const {
    if (auto added = output(git(root_, {"add", "-A", "--", "."})); !added) return std::unexpected(added.error());
    return output(git(root_, args));
}

Result<std::string> IsolatedCheckout::patch() const {
    return stage({"diff", "--cached", "--binary", "--no-ext-diff", baseline_, "--"});
}

Result<> IsolatedCheckout::remove() const {
    if (!backend_) return {};
    std::error_code ec;
    if (!fs::exists(root_, ec)) return {};
    return backend_->remove(repo_, name_, root_);
}

Result<> apply_patch(const fs::path& workspace, std::string_view patch) {
    auto root = toplevel(workspace);
    if (!root) return std::unexpected(root.error());
    for (const bool check : {true, false}) {
        std::vector<std::string> args{"apply"};
        if (check) args.emplace_back("--check");
        args.insert(args.end(), {"--binary", "-"});
        auto out = detail::run(git(*root, std::move(args)), patch);
        if (!out) return std::unexpected(out.error());
        if (!out->success()) return std::unexpected(error::isolation("git apply: " + detail::lossy_trim(out->err)));
    }
    return {};
}

std::vector<PatchFile> patch_files(std::string_view patch) {
    std::vector<PatchFile> files;
    bool in_hunk = false;
    while (!patch.empty()) {
        // Rust `str::lines`: split on `\n`, dropping a trailing `\r`.
        const auto end = patch.find('\n');
        auto line = patch.substr(0, end);
        patch.remove_prefix(end == std::string_view::npos ? patch.size() : end + 1);
        if (line.ends_with('\r')) line.remove_suffix(1);
        if (line.starts_with("diff --git ")) {
            // `a/<path> b/<path>`; the new name is what the change produces.
            auto header = line.substr(11);
            const auto split = header.rfind(" b/");
            files.push_back(PatchFile{std::string(split == std::string_view::npos ? header : header.substr(split + 3)), 0, 0, false});
            in_hunk = false;
            continue;
        }
        if (files.empty()) continue;
        auto& file = files.back();
        if (line.starts_with("@@")) in_hunk = true;
        else if (line == "GIT binary patch" || (line.starts_with("Binary files ") && line.ends_with(" differ"))) file.binary = true;
        else if (in_hunk && line.starts_with('+')) ++file.added;
        else if (in_hunk && line.starts_with('-')) ++file.removed;
    }
    return files;
}

} // namespace loa::sandbox
