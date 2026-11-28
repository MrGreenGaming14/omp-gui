#include "loa/settings/app/error.hpp"

namespace loa::settings::app {

ConfigError ConfigError::invalid(std::string key, std::string reason) {
    ConfigError e;
    e.kind = Kind::Invalid;
    e.key = std::move(key);
    e.reason = std::move(reason);
    return e;
}

std::string ConfigError::to_string() const {
    switch (kind) {
    case Kind::Read: return "can't read " + path.string() + ": " + message;
    case Kind::Write: return "can't write " + path.string() + ": " + message;
    case Kind::Parse: return "the config isn't valid TOML: " + message;
    case Kind::MissingVersion: return "the config has no top level `version` key";
    case Kind::Invalid: return "`" + key + "` " + reason;
    case Kind::TooNew:
        return "the config is schema version " + std::to_string(found) + " but this app understands up to version " +
               std::to_string(supported) + "; update the app to use it";
    case Kind::MissingMigration:
        return "there is no migration from config version " + std::to_string(from) + " to " + std::to_string(from + 1);
    case Kind::Serialize: return "can't serialize the config: " + message;
    }
    return message;
}

loa::Error ConfigError::to_error() const {
    ErrorCode code = ErrorCode::invalid_argument;
    switch (kind) {
    case Kind::Read:
    case Kind::Write: code = ErrorCode::io; break;
    case Kind::Parse: code = ErrorCode::parse; break;
    case Kind::TooNew:
    case Kind::MissingMigration: code = ErrorCode::unsupported; break;
    case Kind::Serialize: code = ErrorCode::internal; break;
    default: break;
    }
    return loa::Error(code, to_string());
}

} // namespace loa::settings::app
