#include <doctest/doctest.h>

#include <string>

#include "loa/core/patch.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/harness/omp/native.hpp"
#include "loa/runtime/json.hpp"

namespace core = loa::core;
namespace json = loa::runtime::json;
namespace omp_native = loa::harness::omp::native;

TEST_CASE("omp native export creates a session header and linked live entries") {
    core::MemorySink sink;
    auto root = sink.commit_at(core::Draft::insert(core::NodeId::DOCUMENT, core::ElementKind::Session,
        {{std::string(core::attr::ID), "new"}, {std::string(core::attr::HARNESS), "omp"},
         {std::string(core::attr::NODE), ""}, {std::string(core::attr::SCHEMA), 1},
         {std::string(core::attr::CREATED), 0}, {std::string(core::attr::CWD), "/repo"}}, ""), 0);
    REQUIRE(root.has_value());
    auto turn = sink.commit_at(core::Draft::insert(root->node, core::ElementKind::Turn,
        {{std::string(core::attr::STATUS), core::TurnStatus::Done}, {std::string(core::attr::STARTED), 1234}}, ""), 1234);
    REQUIRE(turn.has_value());
    auto user = sink.commit_at(core::Draft::insert(turn->node, core::ElementKind::Message,
        {{std::string(core::attr::ROLE), core::Role::User}, {std::string(core::attr::STATUS), core::MessageStatus::Done}},
        "hello"), 1234);
    REQUIRE(user.has_value());
    auto assistant = sink.commit_at(core::Draft::insert(turn->node, core::ElementKind::Message,
        {{std::string(core::attr::ROLE), core::Role::Assistant}, {std::string(core::attr::STATUS), core::MessageStatus::Done},
         {std::string(core::attr::MODEL), "anthropic/claude"}}, "hi"), 1234);
    REQUIRE(assistant.has_value());
    auto exported = omp_native::export_string(sink.dom());
    REQUIRE(exported.has_value());
    auto header_end = exported->find('\n');
    auto user_end = exported->find('\n', header_end + 1);
    REQUIRE(user_end != std::string::npos);
    auto header = json::parse(std::string_view(*exported).substr(0, header_end));
    auto user_line = json::parse(std::string_view(*exported).substr(header_end + 1, user_end - header_end - 1));
    REQUIRE(header.has_value());
    REQUIRE(user_line.has_value());
    CHECK(*header->find("type")->as_string() == "session");
    CHECK(*header->find("id")->as_string() == "new");
    CHECK(*user_line->find("type")->as_string() == "message");
    CHECK(user_line->find("parentId")->is_null());
    CHECK(*user_line->find("message")->find("role")->as_string() == "user");
    core::MemorySink imported;
    CHECK(omp_native::import_str(*exported, imported, "").has_value());
}

TEST_CASE("omp native export rejects a missing session root") {
    const core::Dom empty;
    auto result = omp_native::export_string(empty);
    REQUIRE_FALSE(result.has_value());
    CHECK(result.error().get<loa::contracts::AdapterError::Format>() != nullptr);
}
