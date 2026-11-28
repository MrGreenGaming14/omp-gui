#include "loa/app/workspace.hpp"

#include <fstream>
#include <sstream>

#include <asio.hpp>

#include "loa/app/usage.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::app {

namespace {

namespace fs = std::filesystem;
namespace exec = loa::runtime::exec;

std::optional<std::string> read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::nullopt;
    std::stringstream text;
    text << in.rdbuf();
    return text.str();
}

std::string trim(std::string_view s) {
    const auto first = s.find_first_not_of(" \t\r\n");
    if (first == std::string_view::npos) return {};
    const auto last = s.find_last_not_of(" \t\r\n");
    return std::string(s.substr(first, last - first + 1));
}

// The error of a program the system can't find.
bool not_found(const loa::Error& error) { return error.code() == loa::ErrorCode::not_found; }

} // namespace

std::optional<std::string> GitCli::branch(const fs::path& dir) const {
    std::optional<fs::path> git;
    for (auto at = dir;; at = at.parent_path()) {
        std::error_code ec;
        if (fs::exists(at / ".git", ec)) {
            git = at / ".git";
            break;
        }
        if (at == at.parent_path() || at.empty()) break;
    }
    if (!git) return std::nullopt;
    fs::path git_dir = *git;
    std::error_code ec;
    if (fs::is_regular_file(*git, ec)) {
        // A worktree's `.git` file points at its git dir.
        auto text = read_file(*git);
        if (!text || !text->starts_with("gitdir:")) return std::nullopt;
        fs::path target(trim(std::string_view(*text).substr(7)));
        git_dir = target.is_absolute() ? target : git->parent_path() / target;
    }
    auto head = read_file(git_dir / "HEAD");
    if (!head) return std::nullopt;
    auto trimmed = trim(*head);
    constexpr std::string_view REF = "ref: refs/heads/";
    if (trimmed.starts_with(REF)) return trimmed.substr(REF.size());
    if (trimmed.size() < 7) return std::nullopt;
    return trimmed.substr(0, 7);
}

contracts::Awaitable<std::optional<std::string>> GitCli::diff(fs::path dir) const {
    auto cmd = exec::command("git", {"diff", "--no-color", "--no-ext-diff", "HEAD"}, {}, dir);
    auto output = exec::output(cmd);
    if (!output || !output->status.success()) co_return std::nullopt;
    co_return std::move(output->out);
}

contracts::Awaitable<std::expected<std::optional<fs::path>, std::string>> DialogPicker::pick(fs::path start) const {
#if defined(_WIN32)
    // WinForms' dialog through Windows PowerShell, which every Windows has. The start folder
    // goes in through the environment, so it needs no quoting.
    constexpr const char* SCRIPT = "[Console]::OutputEncoding = [Text.Encoding]::UTF8\n"
                                   "Add-Type -AssemblyName System.Windows.Forms\n"
                                   "$dialog = New-Object System.Windows.Forms.FolderBrowserDialog\n"
                                   "$dialog.Description = 'Choose a folder'\n"
                                   "$dialog.SelectedPath = $env:LOA_FOLDER_START\n"
                                   "$owner = New-Object System.Windows.Forms.Form -Property @{ TopMost = $true }\n"
                                   "if ($dialog.ShowDialog($owner) -eq 'OK') { [Console]::Out.Write($dialog.SelectedPath) }";
    auto cmd = exec::command("powershell.exe", {"-NoProfile", "-NonInteractive", "-STA", "-Command", SCRIPT},
                             {{"LOA_FOLDER_START", start.string()}});
    auto output = exec::output(cmd);
    if (!output) co_return std::unexpected("The folder dialog failed: " + output.error().message());
    if (!output->status.success()) co_return std::unexpected("The folder dialog failed: " + trim(output->err));
    auto chosen = trim(output->out);
    if (chosen.empty()) co_return std::optional<fs::path>{};
    co_return std::optional<fs::path>(chosen);
#else
    const auto from = start.string() + "/";
    const std::pair<const char*, std::vector<std::string>> dialogs[] = {
        {"zenity", {"--file-selection", "--directory", "--title=Choose a folder", "--filename", from}},
        {"kdialog", {"--getexistingdirectory", from, "--title", "Choose a folder"}},
    };
    for (const auto& [program, args] : dialogs) {
        if (!exec::which(program)) continue;
        auto output = exec::output(exec::command(program, args));
        if (!output) {
            if (not_found(output.error())) continue;
            co_return std::unexpected("The folder dialog failed: " + output.error().message());
        }
        // Cancelled.
        if (!output->status.success()) co_return std::optional<fs::path>{};
        auto chosen = trim(output->out);
        if (chosen.empty()) co_return std::optional<fs::path>{};
        co_return std::optional<fs::path>(chosen);
    }
    co_return std::unexpected(std::string("No folder dialog found: install zenity or kdialog, or pick a recent folder"));
#endif
}

loa::view::session::composer::Workspace chip(const GitProbe& git, const fs::path& cwd) {
    loa::view::session::composer::Workspace chip;
    chip.full = cwd.string();
    chip.path = chip.full;
    if (auto home = loa::runtime::fs::home()) {
        auto rel = cwd.lexically_relative(*home);
        const bool inside = !rel.empty() && *rel.begin() != "..";
        if (cwd == *home || rel == ".") chip.path = "~";
        else if (inside) chip.path = "~/" + rel.string();
    }
    chip.branch = git.branch(cwd);
    return chip;
}

// ---- usage -----------------------------------------------------------------------------------

namespace {

// Runs `omp <args>`; its stdout, or its stderr as the error.
std::expected<std::string, std::string> omp(std::vector<std::string> args) {
    const auto what = args.at(0) + " " + args.at(1);
    auto output = exec::output(exec::command("omp", std::move(args)));
    if (!output) return std::unexpected("Couldn't run omp " + what + ": " + output.error().message());
    if (!output->status.success()) return std::unexpected("omp " + what + " failed: " + trim(output->err));
    return std::move(output->out);
}

} // namespace

contracts::Awaitable<std::expected<std::string, std::string>> OmpUsage::fetch() const {
    co_return omp({"usage", "--json"});
}

contracts::Awaitable<std::expected<void, std::string>> OmpUsage::spend(std::string target) const {
    auto out = omp({"usage", "reset", std::move(target)});
    if (!out) co_return std::unexpected(out.error());
    co_return std::expected<void, std::string>{};
}

} // namespace loa::app
