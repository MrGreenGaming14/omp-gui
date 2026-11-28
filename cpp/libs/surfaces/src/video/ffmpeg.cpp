// Port of `loa-surfaces/src/proto/video/ffmpeg.rs`.

#include "loa/surfaces/video/ffmpeg.hpp"

#include <fcntl.h>
#include <signal.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <format>
#include <mutex>
#include <system_error>
#include <thread>
#include <variant>

#include "loa/runtime/exec.hpp"

extern char** environ;

namespace loa::surfaces::video {

namespace {

using Clock = std::chrono::steady_clock;

// A picture still incomplete this long after its last bytes is returned as is.
constexpr auto QUIET = std::chrono::milliseconds(4);

std::string os_error(int code) { return std::format("{} (os error {})", std::generic_category().message(code), code); }

EncoderInfo info(const KnownEncoder& known) {
    return {.name = std::string(NAME_PREFIX) + std::string(known.name), .codec = known.codec, .accel = known.accel};
}

// What the stdout reader thread hands over (Rust `mpsc::channel`).
struct Output {
    std::mutex mutex;
    std::condition_variable ready;
    std::deque<std::variant<std::vector<std::uint8_t>, int>> chunks; // data or errno
    bool disconnected = false;
};

void close_fd(int& fd) {
    if (fd >= 0) ::close(fd);
    fd = -1;
}

// Writes all of `data` without dying of SIGPIPE when the child has exited.
std::optional<int> write_all(int fd, std::span<const std::uint8_t> data) {
    sigset_t pipe_set, old;
    sigemptyset(&pipe_set);
    sigaddset(&pipe_set, SIGPIPE);
    pthread_sigmask(SIG_BLOCK, &pipe_set, &old);
    std::optional<int> failed;
    while (!data.empty()) {
        const ssize_t n = ::write(fd, data.data(), data.size());
        if (n < 0) {
            if (errno == EINTR) continue;
            failed = errno;
            break;
        }
        data = data.subspan(static_cast<std::size_t>(n));
    }
    if (failed == EPIPE && !sigismember(&old, SIGPIPE)) {
        const timespec zero{0, 0};
        while (sigtimedwait(&pipe_set, nullptr, &zero) == SIGPIPE) {}
    }
    pthread_sigmask(SIG_SETMASK, &old, nullptr);
    return failed;
}

} // namespace

// One running ffmpeg; destroying it kills the process.
struct FfmpegEncoder::Process {
    pid_t pid = -1;
    int stdin_fd = -1;
    std::shared_ptr<Output> output;

    void stop() {
        if (pid > 0) {
            ::kill(pid, SIGKILL);
            int status = 0;
            while (::waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
            pid = -1;
        }
    }

    ~Process() {
        stop();
        close_fd(stdin_fd);
    }
};

namespace {

Result<std::unique_ptr<FfmpegEncoder::Process>> start(const std::filesystem::path& binary, std::string_view name, Codec codec,
                                                      Accel accel, std::pair<std::uint32_t, std::uint32_t> size) {
    std::optional<std::filesystem::path> device;
    if (accel == Accel::Vaapi) {
        std::error_code ec;
        for (std::filesystem::directory_iterator it("/dev/dri", ec), end; !ec && it != end; it.increment(ec)) {
            if (it->path().filename().string().starts_with("renderD")) {
                device = it->path();
                break;
            }
        }
        if (!device) return std::unexpected(video_error("no VA-API render device in /dev/dri"));
    }
    std::vector<std::string> args{binary.string()};
    std::ranges::move(start_args(name, codec, accel, size.first, size.second, device), std::back_inserter(args));

    int in[2], out[2];
    if (::pipe2(in, O_CLOEXEC) != 0) return std::unexpected(video_error(std::format("starting {}: {}", name, os_error(errno))));
    if (::pipe2(out, O_CLOEXEC) != 0) {
        const int e = errno;
        ::close(in[0]);
        ::close(in[1]);
        return std::unexpected(video_error(std::format("starting {}: {}", name, os_error(e))));
    }
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_adddup2(&actions, in[0], 0);
    posix_spawn_file_actions_adddup2(&actions, out[1], 1);
    posix_spawn_file_actions_addopen(&actions, 2, "/dev/null", O_WRONLY, 0);
    posix_spawnattr_t attr;
    posix_spawnattr_init(&attr);
    sigset_t defaults;
    sigemptyset(&defaults);
    sigaddset(&defaults, SIGPIPE);
    posix_spawnattr_setsigdefault(&attr, &defaults);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETSIGDEF);
    std::vector<char*> argv;
    for (auto& a : args) argv.push_back(a.data());
    argv.push_back(nullptr);
    pid_t pid = -1;
    auto spawned = runtime::exec::retry_busy([&]() -> std::expected<void, std::error_code> {
        const int rc = ::posix_spawnp(&pid, argv[0], &actions, &attr, argv.data(), environ);
        if (rc != 0) return std::unexpected(std::error_code(rc, std::generic_category()));
        return {};
    });
    posix_spawn_file_actions_destroy(&actions);
    posix_spawnattr_destroy(&attr);
    ::close(in[0]);
    ::close(out[1]);
    if (!spawned) {
        ::close(in[1]);
        ::close(out[0]);
        return std::unexpected(video_error(std::format("starting {}: {}", name, os_error(spawned.error().value()))));
    }
    auto process = std::make_unique<FfmpegEncoder::Process>();
    process->pid = pid;
    process->stdin_fd = in[1];
    process->output = std::make_shared<Output>();
    std::thread([output = process->output, fd = out[0]] {
        std::vector<std::uint8_t> buffer(65536);
        for (;;) {
            const ssize_t n = ::read(fd, buffer.data(), buffer.size());
            if (n < 0 && errno == EINTR) continue;
            std::lock_guard lock(output->mutex);
            if (n > 0) {
                output->chunks.emplace_back(std::vector<std::uint8_t>(buffer.begin(), buffer.begin() + n));
            } else {
                if (n < 0) output->chunks.emplace_back(errno);
                output->disconnected = true;
            }
            output->ready.notify_one();
            if (n <= 0) break;
        }
        ::close(fd);
    }).detach();
    return process;
}

} // namespace

std::optional<FfmpegConfig> FfmpegConfig::discover() {
    if (auto binary = runtime::exec::which("ffmpeg")) return FfmpegConfig{*binary};
    return std::nullopt;
}

std::vector<std::string> start_args(std::string_view name, Codec codec, Accel accel, std::uint32_t width, std::uint32_t height,
                                    const std::optional<std::filesystem::path>& vaapi_device) {
    std::vector<std::string> args{"-hide_banner", "-loglevel", "error", "-nostdin",
                                  "-f", "rawvideo", "-pixel_format", "rgba", "-video_size", std::format("{}x{}", width, height),
                                  "-framerate", std::to_string(FPS), "-i", "pipe:0"};
    auto add = [&](std::initializer_list<std::string_view> more) {
        for (auto a : more) args.emplace_back(a);
    };
    if (accel == Accel::Vaapi && vaapi_device) {
        args.emplace_back("-vaapi_device");
        args.push_back(vaapi_device->string());
    }
    // Default parallelism (VA-API, QSV) holds each picture back until the
    // next one comes in: a frame of delay on a live surface.
    switch (accel) {
    case Accel::Vaapi: add({"-vf", "format=nv12,hwupload", "-async_depth", "1"}); break;
    case Accel::Qsv: add({"-vf", "format=nv12", "-async_depth", "1"}); break;
    case Accel::Nvenc: add({"-delay", "0", "-zerolatency", "1"}); break;
    case Accel::V4l2M2m: add({"-vf", "format=yuv420p"}); break;
    case Accel::Software: break;
    }
    // Every picture goes out as it comes, never held to pace a frame rate.
    add({"-fps_mode", "passthrough"});
    // The in-process H.264 decoder takes Constrained Baseline only.
    if (codec == Codec::H264) {
        switch (accel) {
        case Accel::Vaapi: add({"-profile:v", "constrained_baseline"}); break;
        case Accel::Nvenc:
        case Accel::Qsv: add({"-profile:v", "baseline"}); break;
        case Accel::V4l2M2m:
        case Accel::Software: break;
        }
    }
    const std::string format(nal(codec).name);
    args.insert(args.end(), {"-an", "-c:v", std::string(name), "-g", std::to_string(KEYFRAME_INTERVAL), "-bf", "0", "-q:v",
                             std::to_string(QP), "-flags", "+low_delay", "-bsf:v", format + "_metadata=aud=insert",
                             "-flush_packets", "1", "-f", format, "pipe:1"});
    return args;
}

std::vector<ProbeResult> probe(const FfmpegConfig& config) {
    // HEVC hardware encoders refuse pictures under 128 pixels on a side.
    static constexpr std::uint32_t SIZE = 256;
    auto try_one = [&config](std::string_view name, Codec codec) -> Result<void> {
        auto encoder = FfmpegEncoder::spawn(config, name, {SIZE, SIZE});
        if (!encoder) return std::unexpected(encoder.error());
        auto picture = [](std::uint8_t value) { return std::vector<std::uint8_t>(SIZE * SIZE * 4, value); };
        auto initial = (*encoder)->encode(picture(64), false);
        if (!initial) return std::unexpected(initial.error());
        if (!initial->keyframe || !has_parameter_sets(codec, initial->data))
            return std::unexpected(video_error("first picture lacks a keyframe or parameter sets"));
        auto second = (*encoder)->encode(picture(128), false);
        if (!second) return std::unexpected(second.error());
        if (!has_picture(codec, second->data)) return std::unexpected(video_error("second picture has no coded slice"));
        return {};
    };
    std::array<std::optional<std::string>, KNOWN.size()> errors;
    std::vector<std::thread> tried;
    for (std::size_t i = 0; i < KNOWN.size(); ++i) {
        tried.emplace_back([&, i] {
            try {
                if (auto r = try_one(KNOWN[i].name, KNOWN[i].codec); !r) errors[i] = r.error().message();
            } catch (...) {
                errors[i] = "the probe panicked";
            }
        });
    }
    for (auto& t : tried) t.join();
    std::vector<ProbeResult> found;
    for (std::size_t i = 0; i < KNOWN.size(); ++i) found.push_back({info(KNOWN[i]), errors[i]});
    return found;
}

FfmpegEncoder::~FfmpegEncoder() = default;

Result<std::unique_ptr<FfmpegEncoder>> FfmpegEncoder::spawn(const FfmpegConfig& config, std::string_view name,
                                                            std::pair<std::uint32_t, std::uint32_t> size) {
    if (auto ok = check_video_size(size.first, size.second); !ok) return std::unexpected(ok.error());
    const auto known = std::ranges::find(KNOWN, name, &KnownEncoder::name);
    if (known == KNOWN.end()) return std::unexpected(video_error(std::format("unknown ffmpeg encoder {}", name)));
    auto process = start(config.binary, known->name, known->codec, known->accel, size);
    if (!process) return std::unexpected(process.error());
    std::unique_ptr<FfmpegEncoder> encoder(new FfmpegEncoder());
    encoder->binary_ = config.binary;
    encoder->name_ = known->name;
    encoder->codec_ = known->codec;
    encoder->accel_ = known->accel;
    encoder->size_ = size;
    encoder->process_ = std::move(*process);
    return encoder;
}

ProtoError FfmpegEncoder::error(std::string_view what) const { return video_error(std::format("{}: {}", name_, what)); }

Result<std::vector<std::uint8_t>> FfmpegEncoder::read_picture() {
    const auto deadline = Clock::now() + std::chrono::seconds(2);
    auto last_data = Clock::now();
    auto& output = *process_->output;
    for (;;) {
        if (auto boundary = next_access_unit(codec_, pending_)) {
            std::vector<std::uint8_t> unit(pending_.begin(), pending_.begin() + static_cast<std::ptrdiff_t>(*boundary));
            pending_.erase(pending_.begin(), pending_.begin() + static_cast<std::ptrdiff_t>(*boundary));
            return unit;
        }
        const bool picture = has_picture(codec_, pending_);
        if (picture && Clock::now() - last_data >= QUIET) return std::exchange(pending_, {});
        const auto now = Clock::now();
        if (now >= deadline) return std::unexpected(error("timed out waiting for a coded picture"));
        Clock::duration wait = picture ? std::max<Clock::duration>(QUIET - (now - last_data), Clock::duration::zero()) : deadline - now;
        wait = std::max<Clock::duration>(wait, std::chrono::microseconds(100));
        std::unique_lock lock(output.mutex);
        output.ready.wait_for(lock, wait, [&] { return !output.chunks.empty() || output.disconnected; });
        if (!output.chunks.empty()) {
            auto chunk = std::move(output.chunks.front());
            output.chunks.pop_front();
            lock.unlock();
            if (auto* code = std::get_if<int>(&chunk)) return std::unexpected(error(os_error(*code)));
            auto& data = std::get<std::vector<std::uint8_t>>(chunk);
            pending_.insert(pending_.end(), data.begin(), data.end());
            last_data = Clock::now();
        } else if (output.disconnected) {
            return std::unexpected(error("ffmpeg exited before producing a picture"));
        }
    }
}

Result<AccessUnit> FfmpegEncoder::encode(std::span<const std::uint8_t> rgba, bool keyframe) {
    if (auto ok = check_picture(rgba, size_.first, size_.second); !ok) return std::unexpected(ok.error());
    // The rawvideo pipe can't force an IDR: a fresh process starts with one.
    if (keyframe && frames_ != 0) {
        // Stopped first: a device may take one session at a time.
        process_->stop();
        auto process = start(binary_, name_, codec_, accel_, size_);
        if (!process) return std::unexpected(process.error());
        process_ = std::move(*process);
        frames_ = 0;
    }
    if (auto failed = write_all(process_->stdin_fd, rgba)) return std::unexpected(error(os_error(*failed)));
    auto data = read_picture();
    if (!data) return std::unexpected(data.error());
    ++frames_;
    const bool key = is_keyframe(codec_, *data) && has_parameter_sets(codec_, *data);
    return AccessUnit{.data = std::move(*data), .keyframe = key};
}

std::optional<std::size_t> next_access_unit(Codec codec, std::span<const std::uint8_t> data) {
    const auto& n = nal(codec);
    int seen = 0;
    for (const auto& [code, at] : start_codes(data)) {
        if (at < data.size() && n.type_of(data[at]) == n.aud && ++seen == 2) return code;
    }
    return std::nullopt;
}

} // namespace loa::surfaces::video
