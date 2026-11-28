#include "loa/harness/claude/mapper.hpp"

#include <algorithm>
#include <array>
#include <sstream>

#include "loa/harness/claude/driver.hpp"

namespace loa::harness::claude {
namespace {
using namespace loa::core;
using loa::contracts::AdapterError;
using loa::contracts::Result;
using loa::runtime::json::Array;
using loa::runtime::json::Object;
#define COMMIT(expr) do { auto r_ = (expr); if (!r_) return std::unexpected(AdapterError(AdapterError::Commit{std::move(r_).error()})); } while (false)
#define PROP(expr) do { auto r_ = (expr); if (!r_) return std::unexpected(std::move(r_).error()); } while (false)
const Json& field(const Json& v, std::string_view key) { static const Json null; auto p = v.find(key); return p ? *p : null; }
const std::string* str(const Json& v) { return v.as_string(); }
const std::string* s(const Json& v, std::string_view key) { return str(field(v, key)); }
std::string text(const Json& v, std::string_view key) { auto p = s(v,key); return p ? *p : ""; }
bool yes(const Json& v) { auto b = v.as_bool(); return b && *b; }
std::string kind_of(const Json& b) {
    auto t = s(b,"type"); if (!t) return {};
    if (*t == "text" || *t == "thinking") return *t;
    if (*t == "tool_use" || *t == "server_tool_use" || *t == "mcp_tool_use") return "tool_use";
    return {};
}
std::optional<std::string> content_text(const Json& c) {
    if (auto p = str(c)) return *p;
    auto blocks = c.as_array(); if (!blocks) return {};
    std::string result;
    bool first=true;
    for (const auto& b : *blocks) {
        if (text(b,"type") != "text") return {};
        if (!first) result += '\n';
        first=false;
        result += text(b,"text");
    }
    return result;
}
std::optional<std::string> call_title(const Json& input) {
    for (auto key : {"command","file_path","path","pattern","url","query","description","prompt"}) {
        auto value = s(input,key); if (!value) continue;
        auto line = value->substr(0,value->find('\n'));
        // Rust truncates on Unicode character boundaries, not bytes.
        std::size_t at=0, chars=0;
        while (at<line.size() && chars<120) { unsigned char c=line[at]; at += c<0x80 ? 1 : c<0xe0 ? 2 : c<0xf0 ? 3 : 4; ++chars; }
        if (at<line.size()) return line.substr(0,at)+"…";
        return line;
    }
    return {};
}
std::string msgkey(NodeId id) { return "m:"+std::to_string(id.value); }
std::string callkey(std::string_view id) { return "c:"+std::string(id); }
void put(Attrs& attrs, std::string_view key, Value v) { attrs.emplace(std::string(key),std::move(v)); }
Json obj(std::initializer_list<std::pair<std::string,Json>> pairs) { return Json(Object(pairs)); }
void sort_json(Json& value) {
    if(auto array=value.as_array()) {
        for(auto& child:*array) sort_json(child);
    } else if(auto object=value.as_object()) {
        for(auto& [key,child]:*object) sort_json(child);
        std::sort(object->begin(),object->end(),[](const auto& a,const auto& b){return a.first<b.first;});
    }
}
}

Json control_response(std::string_view request_id, std::expected<Json,std::string> outcome) {
    Json response = outcome ? obj({{"subtype","success"},{"response",std::move(*outcome)}})
                            : obj({{"subtype","error"},{"error",std::move(outcome.error())}});
    response.set("request_id",Json(request_id));
    Json line=obj({{"response",std::move(response)},{"type","control_response"}});
    sort_json(line);
    return line;
}

Mapper::Mapper(std::shared_ptr<PatchSink> sink, NodeId root, std::optional<std::string> native_id,
               std::optional<std::string> model, std::optional<SessionMode> mode)
    : sink_(std::move(sink)), ledger_(root), native_id_(std::move(native_id)), model_(std::move(model)), mode_(mode) {}
DomWriter Mapper::w() const { return DomWriter(*sink_); }
std::optional<NodeId> Mapper::turn() const { auto t=ledger_.turn(); return t ? std::optional(t->node) : std::nullopt; }

Result<Mapper> Mapper::create(std::shared_ptr<PatchSink> sink, const loa::contracts::SessionConfig& config) {
    auto root=kit::insert_root(DomWriter(*sink),HARNESS,config);
    if (!root) return std::unexpected(std::move(root).error());
    return Mapper(std::move(sink),*root,{},config.model,config.mode);
}
Result<Mapper> Mapper::attach(std::shared_ptr<PatchSink> sink, const Dom& dom) {
    auto root=dom.root(); if (!root) return std::unexpected(AdapterError(AdapterError::Format{"the session DOM has no root"}));
    auto optional_str=[&](std::string_view name)->std::optional<std::string>{ auto x=root->str_attr(name); return x ? std::optional(std::string(*x)) : std::nullopt; };
    auto mode=root->str_attr(attr::MODE);
    Mapper mapper(std::move(sink),root->id,optional_str(attr::NATIVE_ID),optional_str(attr::MODEL),mode ? parse<SessionMode>(*mode) : std::nullopt);
    for (auto child:dom.children(root->id)) {
        if(child->kind==ElementKind::Turn) mapper.uncompacted_.push_back(child->id);
        if(child->kind==ElementKind::Compaction) mapper.uncompacted_.clear();
    }
    for (auto turn:dom.children_of_kind(root->id,ElementKind::Turn)) {
        auto status=turn->str_attr(attr::STATUS);
        if(status!="running" && status!="waiting") continue;
        for (const auto& node:dom.descendants(turn->id)) {
            auto st=node.str_attr(attr::STATUS);
            if(node.kind==ElementKind::Message && st=="streaming") COMMIT(mapper.w().set(node.id,attr::STATUS,MessageStatus::Cancelled));
            if(node.kind==ElementKind::Call && (st=="running" || st=="pending")) COMMIT(mapper.w().close(node.id,CallStatus::Cancelled));
            if(node.kind==ElementKind::Decision && st=="waiting") COMMIT(mapper.w().close(node.id,DecisionStatus::Cancelled));
        }
        COMMIT(mapper.w().close(turn->id,TurnStatus::Cancelled));
    }
    return mapper;
}
Result<NodeId> Mapper::begin_turn(std::string_view id) {
    auto result=ledger_.begin_with(w(),nullptr);
    if(!result) return std::unexpected(AdapterError(AdapterError::Commit{std::move(result).error()}));
    NodeId node=*result;
    COMMIT(w().set(node,attr::STARTED,now_ms()));
    if(!id.empty()) COMMIT(w().set(node,attr::ID,std::string(id)));
    uncompacted_.push_back(node);
    return node;
}
Result<NodeId> Mapper::open_turn(std::string_view text, std::string_view by, std::string_view uuid) {
    if(turn_open()) return std::unexpected(AdapterError(AdapterError::Busy{}));
    auto begun=begin_turn(uuid); if(!begun) return std::unexpected(std::move(begun).error());
    COMMIT(w().insert(*begun,ElementKind::Message,Attrs{{std::string(attr::ROLE),Role::User},{std::string(attr::STATUS),MessageStatus::Done},{std::string(attr::ID),std::string(uuid)},{std::string(attr::BY),std::string(by)}},std::string(text)));
    return *begun;
}
Result<std::optional<NodeId>> Mapper::current_turn() {
    if(draining_) return std::nullopt;
    if(auto t=turn()) return t;
    auto r=begin_turn(""); if(!r) return std::unexpected(std::move(r).error()); return std::optional(*r);
}
Result<> Mapper::end_turn(TurnStatus status) {
    if(status==TurnStatus::Done && ledger_.turn()) {
        std::vector<NodeId> messages;
        for(auto& [key,node]:ledger_.turn()->open) if(key.starts_with("m:")) messages.push_back(node);
        for(auto node:messages) { ledger_.take(msgkey(node)); COMMIT(w().set(node,attr::STATUS,MessageStatus::Done)); }
    }
    std::vector<std::pair<std::string,NodeId>> calls;
    auto ended=ledger_.end_turn(w(),status,[&](const kit::EndEvent<Key>& event) {
        if(auto child=std::get_if<kit::end_event::Child<Key>>(&event); child && child->key->starts_with("c:"))
            calls.emplace_back(child->key->substr(2),child->node);
    });
    if(!ended) return std::unexpected(AdapterError(AdapterError::Commit{std::move(ended).error()}));
    for(auto& [id,node]:calls) { calls_.erase(id); COMMIT(w().set(node,attr::ENDED,now_ms())); }
    pending_.clear(); stream_.reset(); return {};
}
Result<> Mapper::record_model(std::string_view model) {
    if(model_!=model) { model_=std::string(model); COMMIT(w().set(ledger_.session,attr::MODEL,std::string(model))); }
    return {};
}
Result<> Mapper::record_mode(SessionMode mode) {
    if(mode_!=mode) { mode_=mode; COMMIT(w().set(ledger_.session,attr::MODE,mode)); }
    return {};
}
Result<> Mapper::interrupt() {
    if(!turn_open()) return {};
    ++draining_; return end_turn(TurnStatus::Cancelled);
}
Result<> Mapper::exited() {
    draining_=0; PROP(end_turn(TurnStatus::Error)); ledger_.close(); return {};
}
Result<std::optional<Output>> Mapper::handle(Json raw) {
    sort_json(raw);
    auto kind=text(raw,"type");
    if(kind=="control_request") return on_control(raw);
    Result<> result;
    if(kind=="system") result=on_system(raw);
    else if(kind=="stream_event" && field(raw,"parent_tool_use_id").is_null()) result=on_stream(field(raw,"event"));
    else if(kind=="assistant") result=on_assistant(raw);
    else if(kind=="user") result=on_user(raw);
    else if(kind=="result") result=on_result(raw);
    else if(kind=="control_cancel_request") { if(auto id=s(raw,"request_id")) result=on_cancel(*id); }
    else if(kind=="stream_event" || kind=="keep_alive" || kind=="tool_progress") return std::nullopt;
    else result=ext(kind,raw);
    if(!result) return std::unexpected(std::move(result).error());
    return std::nullopt;
}
Result<> Mapper::ext(std::string_view kind, const Json& raw) {
    COMMIT(w().ext(turn().value_or(ledger_.session),HARNESS,kind,loa::runtime::json::dump(raw)));
    return {};
}
Result<> Mapper::on_system(const Json& raw) {
    auto subtype=text(raw,"subtype"); auto task=s(raw,"task_id");
    auto mode=s(raw,"permissionMode");
    if(subtype=="init") {
        if(auto id=s(raw,"session_id"); id && native_id_!=*id) { native_id_=*id; COMMIT(w().set(ledger_.session,attr::NATIVE_ID,*id)); }
        if(auto model=s(raw,"model")) PROP(record_model(*model));
    }
    if(subtype=="init" || subtype=="status") {
        if(mode) PROP(record_mode(*mode=="plan" ? SessionMode::Plan : SessionMode::Normal));
        return {};
    }
    if(subtype=="compact_boundary") {
        Value::List covers;
        for(auto node:uncompacted_) if(node!=turn()) covers.emplace_back(node);
        Attrs attrs{{std::string(attr::COVERS),std::move(covers)},{std::string(attr::CREATED),now_ms()}};
        if(auto tokens=field(field(raw,"compact_metadata"),"pre_tokens").as_i64()) put(attrs,attr::TOKENS_BEFORE,*tokens);
        auto inserted=w().insert(ledger_.session,ElementKind::Compaction,std::move(attrs));
        if(!inserted) return std::unexpected(AdapterError(AdapterError::Commit{std::move(inserted).error()}));
        open_compaction_=*inserted;
        std::erase_if(uncompacted_,[&](NodeId n){return n!=turn();}); return {};
    }
    if(subtype=="task_started") return task ? on_task_started(*task,raw) : ext("system",raw);
    if(subtype=="task_notification") {
        auto st=text(raw,"status");
        return end_task(task,st=="completed" ? JobStatus::Done : st=="failed" ? JobStatus::Error : JobStatus::Cancelled,s(raw,"summary"));
    }
    if(subtype=="task_updated") {
        auto st=text(field(raw,"patch"),"status");
        if(st=="completed" || st=="failed" || st=="killed" || st=="stopped")
            return end_task(task,st=="completed" ? JobStatus::Done : st=="failed" ? JobStatus::Error : JobStatus::Cancelled,nullptr);
        return {};
    }
    if(subtype=="task_progress") return {};
    return ext("system",raw);
}
Result<> Mapper::on_task_started(std::string_view task, const Json& raw) {
    auto id=s(raw,"tool_use_id");
    if(id) if(auto it=jobs_.find(*id); it!=jobs_.end()) { jobs_.insert_or_assign(std::string(task),it->second); return {}; }
    auto type=text(raw,"task_type");
    auto kind=type.find("bash")!=std::string::npos || type.find("shell")!=std::string::npos ? JobKind::Shell : JobKind::Subagent;
    std::optional<NodeId> call;
    if(id) if(auto it=calls_.find(*id); it!=calls_.end()) call=it->second.node;
    Attrs attrs{{std::string(attr::KIND),kind},{std::string(attr::STATUS),JobStatus::Running},{std::string(attr::ID),std::string(task)},{std::string(attr::STARTED),now_ms()}};
    if(auto description=s(raw,"description")) put(attrs,attr::TITLE,*description);
    if(call) put(attrs,attr::CALL,*call);
    auto inserted=w().insert(call.value_or(turn().value_or(ledger_.session)),ElementKind::Job,std::move(attrs));
    if(!inserted) return std::unexpected(AdapterError(AdapterError::Commit{std::move(inserted).error()}));
    jobs_.insert_or_assign(std::string(task),*inserted);
    if(id) jobs_.insert_or_assign(*id,*inserted);
    return {};
}
Result<> Mapper::end_task(const std::string* task, JobStatus status, const std::string* summary) {
    if(!task) return {};
    auto it=jobs_.find(*task); if(it==jobs_.end()) return {};
    if(summary && !summary->empty()) COMMIT(w().append(it->second,*summary));
    COMMIT(w().close(it->second,status)); return {};
}

Result<Mapper::StreamBlock> Mapper::open_message(NodeId turn, std::string_view kind,
                                                  const std::optional<std::string>& model, std::string_view content) {
    Attrs attrs{{std::string(attr::ROLE),Role::Assistant},{std::string(attr::STATUS),MessageStatus::Streaming}};
    if(kind=="thinking") put(attrs,attr::KIND,MessageKind::Thinking);
    if(model) put(attrs,attr::MODEL,*model);
    auto node=w().insert(turn,ElementKind::Message,std::move(attrs),std::string(content));
    if(!node) return std::unexpected(AdapterError(AdapterError::Commit{std::move(node).error()}));
    ledger_.open(msgkey(*node),*node);
    return StreamBlock{std::string(kind),*node,*node,false};
}
Result<> Mapper::finish_message(NodeId node) {
    if(ledger_.take(msgkey(node))) COMMIT(w().set(node,attr::STATUS,MessageStatus::Done));
    return {};
}
Result<Mapper::StreamBlock> Mapper::open_call(NodeId turn, std::string_view id,
                                                std::string_view tool, std::string_view input_text) {
    Attrs attrs{{std::string(attr::TOOL),std::string(tool)},{std::string(attr::CALL_ID),std::string(id)},
                {std::string(attr::STATUS),CallStatus::Running},{std::string(attr::STARTED),now_ms()}};
    auto call=w().insert(turn,ElementKind::Call,std::move(attrs));
    if(!call) return std::unexpected(AdapterError(AdapterError::Commit{std::move(call).error()}));
    auto input=w().insert(*call,ElementKind::Input,Attrs{{std::string(attr::FORMAT),"json"}},std::string(input_text));
    if(!input) return std::unexpected(AdapterError(AdapterError::Commit{std::move(input).error()}));
    ledger_.open(callkey(id),*call);
    calls_.insert_or_assign(std::string(id),Call{*call,{}});
    return StreamBlock{"tool_use",*call,*input,false};
}
Result<> Mapper::on_stream(const Json& event) {
    auto kind=text(event,"type");
    if(kind=="message_start") {
        auto current=current_turn(); if(!current) return std::unexpected(std::move(current).error());
        if(*current) {
            auto& body=field(event,"message");
            std::optional<std::string> model;
            if(auto m=s(body,"model")) model=*m;
            stream_=Streaming{text(body,"id"),std::move(model),{},0};
        }
        return {};
    }
    auto index=field(event,"index").as_u64();
    if(!index || *index>SIZE_MAX) return {};
    auto i=static_cast<std::size_t>(*index);
    if(kind=="content_block_start") {
        auto block=event.find("content_block");
        if(!block || !stream_ || !turn()) return {};
        auto block_kind=kind_of(*block);
        std::optional<StreamBlock> entry;
        if(block_kind=="tool_use") {
            auto result=open_call(*turn(),text(*block,"id"),text(*block,"name"),"");
            if(!result) return std::unexpected(std::move(result).error());
            entry=*result;
        } else if(!block_kind.empty()) {
            auto result=open_message(*turn(),block_kind,stream_->model,text(*block,block_kind));
            if(!result) return std::unexpected(std::move(result).error());
            entry=*result;
        }
        // Claude streams index-aligned blocks. A gap is filled with empty entries.
        if(stream_->blocks.size()<i) stream_->blocks.resize(i);
        stream_->blocks.push_back(std::move(entry));
        return {};
    }
    if(!stream_ || i>=stream_->blocks.size() || !stream_->blocks[i]) return {};
    auto& block=*stream_->blocks[i];
    if(kind=="content_block_delta") {
        auto& delta=field(event,"delta");
        auto delta_kind=text(delta,"type");
        auto key=delta_kind=="text_delta" && block.kind=="text" ? "text" :
                 delta_kind=="thinking_delta" && block.kind=="thinking" ? "thinking" :
                 delta_kind=="input_json_delta" && block.kind=="tool_use" ? "partial_json" : "";
        if(!*key) return {};
        auto chunk=text(delta,key);
        if(!chunk.empty()) { block.streamed=true; COMMIT(w().append(block.target,std::move(chunk))); }
    } else if(kind=="content_block_stop" && block.kind!="tool_use") PROP(finish_message(block.node));
    return {};
}
Result<> Mapper::complete_call(NodeId call, const Json& block, const std::string* uuid) {
    auto id=text(block,"id"), tool=text(block,"name");
    auto& input=field(block,"input");
    if(uuid) COMMIT(w().set(call,UUID_ATTR,*uuid));
    if(auto title=call_title(input)) COMMIT(w().set(call,attr::TITLE,*title));
    if((tool!="Task" && tool!="Agent") || jobs_.contains(id)) return {};
    Attrs attrs{{std::string(attr::KIND),JobKind::Subagent},{std::string(attr::STATUS),JobStatus::Running},
                {std::string(attr::CALL),call},{std::string(attr::STARTED),now_ms()}};
    if(auto agent=s(input,"subagent_type")) put(attrs,attr::AGENT,*agent);
    if(auto description=s(input,"description")) put(attrs,attr::TITLE,*description);
    auto job=w().insert(call,ElementKind::Job,std::move(attrs));
    if(!job) return std::unexpected(AdapterError(AdapterError::Commit{std::move(job).error()}));
    jobs_.insert_or_assign(id,*job);
    if(!yes(field(input,"run_in_background"))) if(auto it=calls_.find(id); it!=calls_.end()) it->second.job=*job;
    return {};
}
std::optional<Mapper::StreamBlock> Mapper::next_streamed(std::string_view id, std::string_view kind) {
    if(!stream_ || stream_->message_id!=id) return {};
    while(stream_->finalized<stream_->blocks.size() && !stream_->blocks[stream_->finalized]) ++stream_->finalized;
    if(stream_->finalized>=stream_->blocks.size()) return {};
    auto block=stream_->blocks[stream_->finalized];
    if(!block || block->kind!=kind) return {};
    ++stream_->finalized;
    return block;
}
Result<> Mapper::on_assistant(const Json& raw) {
    auto& body=field(raw,"message");
    auto blocks=field(body,"content").as_array();
    if(auto parent=s(raw,"parent_tool_use_id")) {
        auto it=jobs_.find(*parent); if(it==jobs_.end() || !blocks) return {};
        for(auto& block:*blocks) if(text(block,"type")=="text") if(auto chunk=s(block,"text"); chunk && !chunk->empty())
            COMMIT(w().append(it->second,*chunk+"\n"));
        return {};
    }
    auto current=current_turn(); if(!current) return std::unexpected(std::move(current).error());
    if(!*current || !body.is_object()) return {};
    auto id=text(body,"id");
    auto uuid=s(raw,"uuid");
    if(!blocks) return {};
    for(auto& block:*blocks) {
        auto kind=kind_of(block);
        if(kind.empty()) { COMMIT(w().ext(**current,HARNESS,"block:"+text(block,"type"),loa::runtime::json::dump(block))); continue; }
        auto content=kind=="tool_use" ? loa::runtime::json::dump(field(block,"input")) : text(block,kind);
        NodeId node;
        if(auto streamed=next_streamed(id,kind)) {
            if(!streamed->streamed && !content.empty()) COMMIT(w().set_text(streamed->target,content));
            node=streamed->node;
        } else if(kind=="tool_use") {
            auto opened=open_call(**current,text(block,"id"),text(block,"name"),content);
            if(!opened) return std::unexpected(std::move(opened).error());
            node=opened->node;
        } else {
            std::optional<std::string> model;
            if(auto m=s(body,"model")) model=*m;
            auto opened=open_message(**current,kind,model,content);
            if(!opened) return std::unexpected(std::move(opened).error());
            node=opened->node;
        }
        if(kind=="tool_use") PROP(complete_call(node,block,uuid));
        else { if(uuid) COMMIT(w().set(node,attr::ID,*uuid)); PROP(finish_message(node)); }
    }
    return {};
}

Result<> Mapper::on_user(const Json& raw) {
    if(!field(raw,"parent_tool_use_id").is_null() || draining_ || yes(field(raw,"isReplay"))) return {};
    auto& content=field(field(raw,"message"),"content");
    if(content.is_null()) return {};
    if(open_compaction_ && yes(field(raw,"isCompactSummary"))) {
        auto compact=*open_compaction_; open_compaction_.reset();
        COMMIT(w().set_text(compact,content_text(content).value_or(loa::runtime::json::dump(content))));
        return {};
    }
    auto blocks=content.as_array();
    if(!blocks) return ext("user",raw);
    Array rest;
    for(auto& block:*blocks) {
        if(text(block,"type")=="tool_result") PROP(on_tool_result(block,s(raw,"uuid")));
        else rest.push_back(block);
    }
    if(rest.empty()) return {};
    Json remainder=raw;
    Json message=field(remainder,"message");
    message.set("content",Json(std::move(rest)));
    remainder.set("message",std::move(message));
    return ext("user",remainder);
}
Result<> Mapper::on_tool_result(const Json& block, const std::string* uuid) {
    auto id=text(block,"tool_use_id");
    auto it=calls_.find(id);
    if(it==calls_.end()) return ext("tool_result",block);
    auto& content=field(block,"content");
    auto converted=content_text(content);
    bool json=!content.is_null() && !converted;
    auto body=content.is_null() ? std::string{} : converted.value_or(loa::runtime::json::dump(content));
    bool error=yes(field(block,"is_error"));
    Attrs attrs{{std::string(attr::IS_ERROR),error}};
    if(uuid) put(attrs,UUID_ATTR,*uuid);
    if(json) put(attrs,attr::FORMAT,"json");
    COMMIT(w().insert(it->second.node,ElementKind::Result,std::move(attrs),std::move(body)));
    COMMIT(w().close(it->second.node,error ? CallStatus::Error : CallStatus::Done));
    ledger_.take(callkey(id));
    if(it->second.job) COMMIT(w().close(*it->second.job,error ? JobStatus::Error : JobStatus::Done));
    return {};
}
Result<> Mapper::on_result(const Json& raw) {
    if(draining_) { --draining_; return {}; }
    auto current=turn(); if(!current) return {};
    Attrs attrs;
    auto& usage=field(raw,"usage");
    for(auto [name,key]:std::array<std::pair<std::string_view,std::string_view>,4>{
        {{attr::INPUT_TOKENS,"input_tokens"},{attr::OUTPUT_TOKENS,"output_tokens"},
         {attr::CACHE_READ_TOKENS,"cache_read_input_tokens"},{attr::CACHE_WRITE_TOKENS,"cache_creation_input_tokens"}}}) {
        if(auto n=field(usage,key).as_i64()) put(attrs,name,*n);
    }
    if(auto n=field(raw,"duration_ms").as_i64()) put(attrs,attr::DURATION_MS,*n);
    if(auto cost=field(raw,"total_cost_usd").as_f64()) put(attrs,attr::COST,*cost);
    if(model_) put(attrs,attr::MODEL,*model_);
    if(!attrs.empty()) COMMIT(w().insert(*current,ElementKind::Usage,std::move(attrs)));
    auto reason=s(raw,"terminal_reason"), subtype=s(raw,"subtype");
    auto status=reason && reason->starts_with("aborted") ? TurnStatus::Cancelled :
                yes(field(raw,"is_error")) || (subtype && *subtype!="success") ? TurnStatus::Error : TurnStatus::Done;
    return end_turn(status);
}
Result<std::optional<Output>> Mapper::on_control(const Json& raw) {
    auto request_id=text(raw,"request_id");
    auto& request=field(raw,"request");
    auto subtype=text(request,"subtype");
    if(request.is_null()) return std::optional<Output>(Write{control_response(request_id,std::unexpected(std::string("missing request")))});
    if(subtype=="can_use_tool") return on_can_use_tool(request_id,request);
    if(subtype=="mcp_message") return std::optional<Output>(Mcp{request_id,text(request,"server_name"),field(request,"message")});
    return std::optional<Output>(Write{control_response(request_id,std::unexpected("unsupported control request `"+subtype+"`"))});
}
Result<std::optional<Output>> Mapper::on_can_use_tool(std::string_view request_id, const Json& request) {
    auto current=current_turn(); if(!current) return std::unexpected(std::move(current).error());
    if(!*current) {
        auto deny=obj({{"behavior","deny"},{"message","The turn was interrupted."}});
        return std::optional<Output>(Write{control_response(request_id,std::move(deny))});
    }
    auto tool=text(request,"tool_name");
    auto& input=field(request,"input");
    std::optional<NodeId> call;
    if(auto id=s(request,"tool_use_id")) if(auto it=calls_.find(*id); it!=calls_.end()) call=it->second.node;
    auto kind=tool=="AskUserQuestion" ? DecisionKind::Question : tool=="ExitPlanMode" ? DecisionKind::Plan : DecisionKind::Approval;
    auto base=[&](std::optional<std::string> title) {
        Attrs attrs{{std::string(attr::KIND),kind},{std::string(attr::STATUS),DecisionStatus::Waiting},
                    {std::string(attr::REQUEST_ID),std::string(request_id)},{std::string(attr::CREATED),now_ms()}};
        if(call) put(attrs,attr::CALL,*call);
        if(title) put(attrs,attr::TITLE,*title);
        return attrs;
    };
    std::vector<std::pair<Attrs,std::string>> asks;
    if(kind==DecisionKind::Question) {
        if(auto questions=field(input,"questions").as_array()) for(auto& question:*questions) {
            std::optional<std::string> title; if(auto t=s(question,"header")) title=*t;
            auto attrs=base(std::move(title));
            Value::List options;
            if(auto items=field(question,"options").as_array()) for(auto& option:*items)
                if(auto label=s(option,"label")) options.emplace_back(*label);
            put(attrs,attr::OPTIONS,std::move(options));
            put(attrs,attr::MULTI,yes(field(question,"multiSelect")));
            asks.emplace_back(std::move(attrs),text(question,"question"));
        }
    } else if(kind==DecisionKind::Plan) {
        asks.emplace_back(base(std::string("Plan")),text(input,"plan"));
    } else {
        auto title=s(request,"title");
        auto attrs=base(title ? *title : "Allow "+tool+"?");
        put(attrs,attr::OPTIONS,Value::List{Value(answer::ALLOW_ONCE),Value(answer::ALLOW_SESSION),Value(answer::DENY)});
        if(auto reason=s(request,"decision_reason")) put(attrs,"x-claude-decision-reason",*reason);
        if(auto path=s(request,"blocked_path")) put(attrs,"x-claude-blocked-path",*path);
        auto description=s(request,"description");
        asks.emplace_back(std::move(attrs),description ? *description : loa::runtime::json::dump_pretty(input));
    }
    Pending pending{std::string(request_id),kind,std::move(tool),input,{},{}};
    if(auto suggestions=request.find("permission_suggestions"); suggestions && !suggestions->is_null()) pending.suggestions=*suggestions;
    for(auto& [attrs,body]:asks) {
        auto node=w().insert(call.value_or(**current),ElementKind::Decision,std::move(attrs),std::move(body));
        if(!node) return std::unexpected(AdapterError(AdapterError::Commit{std::move(node).error()}));
        ledger_.wait(*node);
        pending.decisions.emplace_back(*node,std::nullopt);
    }
    if(pending.decisions.empty()) return std::optional<Output>(Write{control_response(request_id,permission_response(pending))});
    pending_.push_back(std::move(pending));
    COMMIT(w().set(**current,attr::STATUS,TurnStatus::Waiting));
    return std::nullopt;
}
Result<> Mapper::on_cancel(std::string_view request_id) {
    auto it=std::find_if(pending_.begin(),pending_.end(),[&](const Pending& p){return p.request_id==request_id;});
    if(it==pending_.end()) return {};
    auto decisions=std::move(it->decisions);
    pending_.erase(it);
    for(auto& [node,answer]:decisions) if(!answer) {
        COMMIT(w().close(node,DecisionStatus::Cancelled));
        ledger_.answered(node);
    }
    return resume_turn();
}
Result<> Mapper::resume_turn() {
    if(auto current=turn(); current && pending_.empty()) COMMIT(w().set(*current,attr::STATUS,TurnStatus::Running));
    return {};
}
Json Mapper::permission_response(const Pending& pending) {
    auto first=pending.decisions.empty() ? nullptr : &pending.decisions.front().second;
    auto answer=first && *first ? &**first : nullptr;
    auto value=answer && !answer->values.empty() ? answer->values.front() : std::string{};
    auto reason=answer && answer->reason ? *answer->reason : std::string{};
    Json input=pending.input;
    auto allow=[&]() { return obj({{"behavior","allow"},{"updatedInput",input}}); };
    if(pending.kind==DecisionKind::Approval) {
        if(value==answer::ALLOW_SESSION) {
            Json rules=pending.suggestions.value_or(Json(Array{obj({{"type","addRules"},
                {"rules",Array{obj({{"toolName",pending.tool}})}},{"behavior","allow"},{"destination","session"}})}));
            return obj({{"behavior","allow"},{"updatedInput",input},{"updatedPermissions",rules}});
        }
        if(value==answer::ALLOW_ONCE) return allow();
        return obj({{"behavior","deny"},{"message",reason.empty() ? "The user denied this tool call." : reason}});
    }
    if(pending.kind==DecisionKind::Plan) {
        if(value==answer::APPROVE) return allow();
        if(value==answer::EDIT) {
            if(answer && answer->reason && input.is_object()) input.set("plan",*answer->reason);
            return allow();
        }
        return obj({{"behavior","deny"},{"message",reason.empty() ? "The user rejected the plan." : reason}});
    }
    Object answers;
    auto questions=field(pending.input,"questions").as_array();
    for(std::size_t i=0;i<pending.decisions.size();++i) {
        if(!questions || i>=questions->size() || !pending.decisions[i].second) continue;
        auto question=s((*questions)[i],"question"); if(!question) continue;
        std::string joined;
        for(auto& v:pending.decisions[i].second->values) { if(!joined.empty()) joined+=", "; joined+=v; }
        answers.emplace_back(*question,std::move(joined));
    }
    if(input.is_object()) input.set("answers",Json(std::move(answers)));
    return allow();
}
Result<std::optional<Json>> Mapper::answer(NodeId decision, const loa::contracts::Answer& response) {
    std::size_t at=0,slot=0; bool found=false;
    for(;at<pending_.size() && !found;++at)
        for(slot=0;slot<pending_[at].decisions.size();++slot)
            if(pending_[at].decisions[slot].first==decision && !pending_[at].decisions[slot].second) {found=true;break;}
    if(!found) return std::unexpected(AdapterError(AdapterError::UnknownDecision{decision}));
    --at;
    auto kind=pending_[at].kind;
    if(kind==DecisionKind::Approval || kind==DecisionKind::Plan) {
        auto allowed=kind==DecisionKind::Approval ?
            std::array<std::string_view,3>{answer::ALLOW_ONCE,answer::ALLOW_SESSION,answer::DENY} :
            std::array<std::string_view,3>{answer::APPROVE,answer::EDIT,answer::REJECT};
        if(response.values.empty() || std::find(allowed.begin(),allowed.end(),response.values.front())==allowed.end())
            return std::unexpected(AdapterError(AdapterError::Protocol{"the answer must be one of the decision options"}));
    }
    COMMIT(w().set(decision,attr::ANSWER,Value(response.values)));
    COMMIT(w().set(decision,attr::BY,response.by));
    if(response.reason) COMMIT(w().set(decision,attr::REASON,*response.reason));
    COMMIT(w().close(decision,DecisionStatus::Answered));
    ledger_.answered(decision);
    pending_[at].decisions[slot].second=response;
    for(auto& [node,answer]:pending_[at].decisions) if(!answer) return std::nullopt;
    auto pending=std::move(pending_[at]); pending_.erase(pending_.begin()+at);
    auto reply=permission_response(pending);
    if(pending.kind==DecisionKind::Plan && text(reply,"behavior")=="allow") PROP(record_mode(SessionMode::Normal));
    PROP(resume_turn());
    return std::optional<Json>(control_response(pending.request_id,std::move(reply)));
}

} // namespace loa::harness::claude
