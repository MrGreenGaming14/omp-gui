//! Element schema v1.
//!
//! The schema is data: one [`ElementSpec`] per [`ElementKind`] listing the
//! allowed parents and attributes. [`Dom::apply`](crate::Dom::apply) enforces
//! it on every `insert` and `set`.
//!
//! Extension rules, so adapters never need a schema change to keep data:
//! - any element accepts attributes whose name starts with `x-`, of any type
//! - `<ext>` accepts any attribute and may sit under any element

use crate::value::{Value, ValueType};
use serde::{Deserialize, Serialize};

/// Version of the element schema. Adapters declare the version they emit and
/// `<session schema="…">` records the version a journal was written with.
pub const SCHEMA_VERSION: u32 = 1;

/// Reserved attribute name: a `set` on it replaces (or clears) the node text.
pub const TEXT: &str = "#text";

/// Prefix of attributes that bypass the schema.
pub const EXTENSION_PREFIX: &str = "x-";

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub const VALUES: &'static [&'static str] = &[$($text),+];

            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some($name::$variant),)+ _ => None }
            }
        }

        impl From<$name> for Value {
            fn from(v: $name) -> Self {
                Value::Str(v.as_str().to_owned())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum! {
    /// Every element of schema v1. `as_str` is the tag name.
    ElementKind {
        Session => "session",
        Turn => "turn",
        Message => "message",
        Call => "call",
        Input => "input",
        Result => "result",
        Usage => "usage",
        Decision => "decision",
        Job => "job",
        Todo => "todo",
        Surface => "surface",
        Compaction => "compaction",
        Digest => "digest",
        Ext => "ext",
    }
}

str_enum! { Role { User => "user", Assistant => "assistant", System => "system" } }
str_enum! { MessageKind { Text => "text", Thinking => "thinking" } }
str_enum! { MessageStatus { Streaming => "streaming", Done => "done", Error => "error", Cancelled => "cancelled" } }
str_enum! { TurnStatus { Running => "running", Waiting => "waiting", Done => "done", Error => "error", Cancelled => "cancelled" } }
str_enum! { CallStatus { Pending => "pending", Running => "running", Done => "done", Error => "error", Cancelled => "cancelled" } }
str_enum! { DecisionKind { Approval => "approval", Question => "question", Plan => "plan", Form => "form" } }
str_enum! { DecisionStatus { Waiting => "waiting", Answered => "answered", Expired => "expired", Cancelled => "cancelled" } }
str_enum! { JobKind { Shell => "shell", Subagent => "subagent" } }
str_enum! { JobStatus { Running => "running", Done => "done", Error => "error", Cancelled => "cancelled" } }
str_enum! { TodoState { Pending => "pending", InProgress => "in_progress", Done => "done", Cancelled => "cancelled" } }
str_enum! { SurfaceMode { Pixel => "pixel", Document => "document" } }
str_enum! { SurfaceStatus { Open => "open", Closed => "closed" } }
str_enum! { SessionMode { Normal => "normal", Plan => "plan" } }

/// Canonical `answer` values written on `<decision>` elements.
pub mod answer {
    /// Approval: run this call once.
    pub const ALLOW_ONCE: &str = "allow_once";
    /// Approval: allow this tool for the rest of the session.
    pub const ALLOW_SESSION: &str = "allow_session";
    /// Approval: refuse. `reason` may hold why.
    pub const DENY: &str = "deny";
    /// Plan review: run the plan as written.
    pub const APPROVE: &str = "approve";
    /// Plan review: run the edited plan stored in the decision text.
    pub const EDIT: &str = "edit";
    /// Plan review: refuse. `reason` holds the feedback.
    pub const REJECT: &str = "reject";
}

/// Attribute names of schema v1.
pub mod attr {
    pub const ID: &str = "id";
    pub const SCHEMA: &str = "schema";
    pub const HARNESS: &str = "harness";
    pub const PROJECT: &str = "project";
    pub const NODE: &str = "node";
    pub const CREATED: &str = "created";
    pub const TITLE: &str = "title";
    pub const CWD: &str = "cwd";
    pub const MODEL: &str = "model";
    pub const ROLE: &str = "role";
    pub const MODEL_ROLE: &str = "model_role";
    pub const EFFORT: &str = "effort";
    pub const MODE: &str = "mode";
    pub const PARENT_SESSION: &str = "parent_session";
    pub const FORKED_AT: &str = "forked_at";
    pub const NATIVE_ID: &str = "native_id";
    pub const EXCLUDED: &str = "excluded";
    pub const STATUS: &str = "status";
    pub const STARTED: &str = "started";
    pub const ENDED: &str = "ended";
    pub const KIND: &str = "kind";
    pub const TOOL: &str = "tool";
    pub const CALL_ID: &str = "call_id";
    pub const FORMAT: &str = "format";
    pub const IS_ERROR: &str = "is_error";
    pub const TRUNCATED: &str = "truncated";
    pub const INPUT_TOKENS: &str = "input_tokens";
    pub const OUTPUT_TOKENS: &str = "output_tokens";
    pub const CACHE_READ_TOKENS: &str = "cache_read_tokens";
    pub const CACHE_WRITE_TOKENS: &str = "cache_write_tokens";
    pub const COST: &str = "cost";
    pub const DURATION_MS: &str = "duration_ms";
    pub const OPTIONS: &str = "options";
    pub const MULTI: &str = "multi";
    pub const ANSWER: &str = "answer";
    pub const BY: &str = "by";
    pub const REASON: &str = "reason";
    pub const CALL: &str = "call";
    pub const DEFAULT: &str = "default";
    pub const TIMEOUT_MS: &str = "timeout_ms";
    pub const REQUEST_ID: &str = "request_id";
    pub const FIELDS: &str = "fields";
    pub const COMMAND: &str = "command";
    pub const SESSION: &str = "session";
    pub const AGENT: &str = "agent";
    pub const EXIT_CODE: &str = "exit_code";
    pub const STATE: &str = "state";
    pub const PHASE: &str = "phase";
    pub const URI: &str = "uri";
    pub const NAME: &str = "name";
    pub const WIDTH: &str = "width";
    pub const HEIGHT: &str = "height";
    pub const SERVER: &str = "server";
    pub const COVERS: &str = "covers";
    pub const TOKENS_BEFORE: &str = "tokens_before";
    pub const SOURCE_SEQ: &str = "source_seq";
    pub const STALE: &str = "stale";
    pub const DECISIONS: &str = "decisions";
    pub const FILES: &str = "files";
    pub const OPEN_ITEMS: &str = "open_items";
    pub const POINTERS: &str = "pointers";
    pub const TYPE: &str = "type";
    pub const RAW: &str = "raw";
}

/// One attribute an element accepts.
#[derive(Debug)]
pub struct AttrSpec {
    pub name: &'static str,
    pub ty: ValueType,
    /// Must be present on insert and can't be cleared.
    pub required: bool,
    /// Closed set of allowed strings. Empty means any value of `ty`.
    pub values: &'static [&'static str],
}

/// The schema of one element.
#[derive(Debug)]
pub struct ElementSpec {
    pub kind: ElementKind,
    /// Kinds this element may sit under. Empty means document root only.
    pub parents: &'static [ElementKind],
    pub attrs: &'static [AttrSpec],
    /// Accepts attributes that are not listed in `attrs`.
    pub open: bool,
}

const fn opt(name: &'static str, ty: ValueType) -> AttrSpec {
    AttrSpec { name, ty, required: false, values: &[] }
}

const fn req(name: &'static str, ty: ValueType) -> AttrSpec {
    AttrSpec { name, ty, required: true, values: &[] }
}

const fn opt_enum(name: &'static str, values: &'static [&'static str]) -> AttrSpec {
    AttrSpec { name, ty: ValueType::Str, required: false, values }
}

const fn req_enum(name: &'static str, values: &'static [&'static str]) -> AttrSpec {
    AttrSpec { name, ty: ValueType::Str, required: true, values }
}

use ElementKind as K;
use ValueType as T;

static SESSION: ElementSpec = ElementSpec {
    kind: K::Session,
    parents: &[],
    attrs: &[
        // App-wide session id (UUID).
        req(attr::ID, T::Str),
        // Adapter id that runs the session, e.g. "omp", "claude-code".
        req(attr::HARNESS, T::Str),
        // Public key of the owner node, the single writer.
        req(attr::NODE, T::Str),
        // Unix milliseconds.
        req(attr::CREATED, T::Int),
        opt(attr::SCHEMA, T::Int),
        opt(attr::PROJECT, T::Str),
        opt(attr::TITLE, T::Str),
        opt(attr::CWD, T::Str),
        // Current model, model role, effort and mode. Changes are `set` patches.
        opt(attr::MODEL, T::Str),
        opt(attr::MODEL_ROLE, T::Str),
        opt(attr::EFFORT, T::Str),
        opt_enum(attr::MODE, SessionMode::VALUES),
        // Fork lineage: source session id and the patch sequence it forked at.
        opt(attr::PARENT_SESSION, T::Str),
        opt(attr::FORKED_AT, T::Int),
        // The harness's own session id or file.
        opt(attr::NATIVE_ID, T::Str),
        // Left out of project context when true.
        opt(attr::EXCLUDED, T::Bool),
    ],
    open: false,
};

static TURN: ElementSpec = ElementSpec {
    kind: K::Turn,
    parents: &[K::Session],
    attrs: &[
        opt(attr::ID, T::Str),
        opt_enum(attr::STATUS, TurnStatus::VALUES),
        opt(attr::STARTED, T::Int),
        opt(attr::ENDED, T::Int),
    ],
    open: false,
};

static MESSAGE: ElementSpec = ElementSpec {
    kind: K::Message,
    parents: &[K::Turn],
    attrs: &[
        req_enum(attr::ROLE, Role::VALUES),
        // Absent means "text".
        opt_enum(attr::KIND, MessageKind::VALUES),
        opt(attr::ID, T::Str),
        opt(attr::MODEL, T::Str),
        opt_enum(attr::STATUS, MessageStatus::VALUES),
        // Node key or user that wrote a user message.
        opt(attr::BY, T::Str),
    ],
    open: false,
};

static CALL: ElementSpec = ElementSpec {
    kind: K::Call,
    parents: &[K::Turn],
    attrs: &[
        req(attr::TOOL, T::Str),
        opt_enum(attr::STATUS, CallStatus::VALUES),
        // The harness's id for the tool call.
        opt(attr::CALL_ID, T::Str),
        // One line shown on the collapsed row.
        opt(attr::TITLE, T::Str),
        opt(attr::STARTED, T::Int),
        opt(attr::ENDED, T::Int),
    ],
    open: false,
};

static INPUT: ElementSpec = ElementSpec {
    kind: K::Input,
    parents: &[K::Call],
    // Text holds the arguments. `format` is "json" unless stated.
    attrs: &[opt(attr::FORMAT, T::Str)],
    open: false,
};

static RESULT: ElementSpec = ElementSpec {
    kind: K::Result,
    parents: &[K::Call],
    attrs: &[
        opt(attr::FORMAT, T::Str),
        opt(attr::IS_ERROR, T::Bool),
        opt(attr::TRUNCATED, T::Bool),
    ],
    open: false,
};

static USAGE: ElementSpec = ElementSpec {
    kind: K::Usage,
    parents: &[K::Call, K::Message, K::Turn, K::Job],
    attrs: &[
        opt(attr::INPUT_TOKENS, T::Int),
        opt(attr::OUTPUT_TOKENS, T::Int),
        opt(attr::CACHE_READ_TOKENS, T::Int),
        opt(attr::CACHE_WRITE_TOKENS, T::Int),
        opt(attr::COST, T::Float),
        opt(attr::DURATION_MS, T::Int),
        opt(attr::MODEL, T::Str),
    ],
    open: false,
};

static DECISION: ElementSpec = ElementSpec {
    kind: K::Decision,
    parents: &[K::Turn, K::Call, K::Session],
    // Text holds the body: command or diff preview, question, plan markdown.
    attrs: &[
        req_enum(attr::KIND, DecisionKind::VALUES),
        opt_enum(attr::STATUS, DecisionStatus::VALUES),
        opt(attr::TITLE, T::Str),
        // List of option labels.
        opt(attr::OPTIONS, T::List),
        // Several options may be picked.
        opt(attr::MULTI, T::Bool),
        // List of strings: picked options, typed text, or a value from `answer`.
        opt(attr::ANSWER, T::List),
        // Node key or user that answered.
        opt(attr::BY, T::Str),
        opt(attr::REASON, T::Str),
        // Node id of the `<call>` an approval is for.
        opt(attr::CALL, T::Int),
        // Unattended runs: answer applied when `timeout_ms` passes.
        opt(attr::DEFAULT, T::List),
        opt(attr::TIMEOUT_MS, T::Int),
        // The harness's id for the request.
        opt(attr::REQUEST_ID, T::Str),
        // Form: JSON Schema of the fields, as text.
        opt(attr::FIELDS, T::Str),
        opt(attr::CREATED, T::Int),
        opt(attr::ENDED, T::Int),
    ],
    open: false,
};

static JOB: ElementSpec = ElementSpec {
    kind: K::Job,
    parents: &[K::Turn, K::Call, K::Session],
    // Text holds output so far.
    attrs: &[
        req_enum(attr::KIND, JobKind::VALUES),
        opt_enum(attr::STATUS, JobStatus::VALUES),
        opt(attr::ID, T::Str),
        opt(attr::TITLE, T::Str),
        opt(attr::COMMAND, T::Str),
        // Subagent: session id of the linked child session.
        opt(attr::SESSION, T::Str),
        opt(attr::AGENT, T::Str),
        opt(attr::EXIT_CODE, T::Int),
        // Node id of the `<call>` that started the job.
        opt(attr::CALL, T::Int),
        opt(attr::STARTED, T::Int),
        opt(attr::ENDED, T::Int),
    ],
    open: false,
};

static TODO: ElementSpec = ElementSpec {
    kind: K::Todo,
    parents: &[K::Turn, K::Session],
    attrs: &[
        opt_enum(attr::STATE, TodoState::VALUES),
        opt(attr::ID, T::Str),
        opt(attr::PHASE, T::Str),
    ],
    open: false,
};

static SURFACE: ElementSpec = ElementSpec {
    kind: K::Surface,
    parents: &[K::Turn, K::Call, K::Session],
    attrs: &[
        req(attr::URI, T::Str),
        req_enum(attr::MODE, SurfaceMode::VALUES),
        opt(attr::NAME, T::Str),
        opt(attr::WIDTH, T::Int),
        opt(attr::HEIGHT, T::Int),
        opt(attr::SERVER, T::Str),
        opt_enum(attr::STATUS, SurfaceStatus::VALUES),
    ],
    open: false,
};

static COMPACTION: ElementSpec = ElementSpec {
    kind: K::Compaction,
    parents: &[K::Session],
    // Text holds the summary.
    attrs: &[
        // Node ids (Int) of the turns the summary replaces.
        opt(attr::COVERS, T::List),
        opt(attr::TOKENS_BEFORE, T::Int),
        opt(attr::MODEL, T::Str),
        opt(attr::CREATED, T::Int),
    ],
    open: false,
};

static DIGEST: ElementSpec = ElementSpec {
    kind: K::Digest,
    parents: &[K::Session],
    // Text holds the two or three sentence summary.
    attrs: &[
        // Patch sequence the digest was built from.
        req(attr::SOURCE_SEQ, T::Int),
        opt(attr::STALE, T::Bool),
        opt(attr::DECISIONS, T::List),
        // "path: note" strings.
        opt(attr::FILES, T::List),
        opt(attr::OPEN_ITEMS, T::List),
        // Node ids (Int) of the turns the digest came from.
        opt(attr::POINTERS, T::List),
        opt(attr::MODEL, T::Str),
        opt(attr::CREATED, T::Int),
    ],
    open: false,
};

static EXT: ElementSpec = ElementSpec {
    kind: K::Ext,
    parents: ElementKind::ALL,
    // `raw` or the text keeps the harness's own data so round trips are lossless.
    attrs: &[req(attr::HARNESS, T::Str), opt(attr::TYPE, T::Str)],
    open: true,
};

impl ElementKind {
    pub fn spec(self) -> &'static ElementSpec {
        match self {
            K::Session => &SESSION,
            K::Turn => &TURN,
            K::Message => &MESSAGE,
            K::Call => &CALL,
            K::Input => &INPUT,
            K::Result => &RESULT,
            K::Usage => &USAGE,
            K::Decision => &DECISION,
            K::Job => &JOB,
            K::Todo => &TODO,
            K::Surface => &SURFACE,
            K::Compaction => &COMPACTION,
            K::Digest => &DIGEST,
            K::Ext => &EXT,
        }
    }
}

impl ElementSpec {
    pub fn attr(&self, name: &str) -> Option<&'static AttrSpec> {
        self.attrs.iter().find(|a| a.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum SchemaError {
    #[error("<{child}> can't sit under {}", parent.map_or("the document".to_owned(), |p| format!("<{p}>")))]
    Parent { child: ElementKind, parent: Option<ElementKind> },
    #[error("<{kind}> has no attribute `{name}`")]
    UnknownAttr { kind: ElementKind, name: String },
    #[error("<{kind} {name}> expects {expected:?}, got {got:?}")]
    Type { kind: ElementKind, name: String, expected: ValueType, got: ValueType },
    #[error("<{kind} {name}> doesn't allow `{value}`")]
    EnumValue { kind: ElementKind, name: String, value: String },
    #[error("<{kind}> requires attribute `{name}`")]
    Required { kind: ElementKind, name: &'static str },
    #[error("`{}` takes a string", TEXT)]
    TextType,
}

/// Checks that `kind` may be inserted under `parent` (`None` is the document)
/// with `attrs`.
pub fn validate_insert<'a>(
    parent: Option<ElementKind>,
    kind: ElementKind,
    attrs: impl IntoIterator<Item = (&'a str, &'a Value)> + Clone,
) -> Result<(), SchemaError> {
    let spec = kind.spec();
    let parent_ok = match parent {
        None => spec.parents.is_empty(),
        Some(p) => spec.parents.contains(&p),
    };
    if !parent_ok {
        return Err(SchemaError::Parent { child: kind, parent });
    }
    for (name, value) in attrs.clone() {
        validate_attr(spec, name, value)?;
    }
    for required in spec.attrs.iter().filter(|a| a.required) {
        if !attrs.clone().into_iter().any(|(name, _)| name == required.name) {
            return Err(SchemaError::Required { kind, name: required.name });
        }
    }
    Ok(())
}

/// Checks a `set` of `name` on an element of `kind`. `None` clears.
pub fn validate_set(kind: ElementKind, name: &str, value: Option<&Value>) -> Result<(), SchemaError> {
    if name == TEXT {
        return match value {
            None | Some(Value::Str(_)) => Ok(()),
            Some(_) => Err(SchemaError::TextType),
        };
    }
    let spec = kind.spec();
    match value {
        Some(value) => validate_attr(spec, name, value),
        None => match spec.attr(name) {
            Some(a) if a.required => Err(SchemaError::Required { kind, name: a.name }),
            Some(_) => Ok(()),
            None if spec.open || name.starts_with(EXTENSION_PREFIX) => Ok(()),
            None => Err(SchemaError::UnknownAttr { kind, name: name.to_owned() }),
        },
    }
}

fn validate_attr(spec: &ElementSpec, name: &str, value: &Value) -> Result<(), SchemaError> {
    let kind = spec.kind;
    let Some(attr) = spec.attr(name) else {
        if spec.open || name.starts_with(EXTENSION_PREFIX) {
            return Ok(());
        }
        return Err(SchemaError::UnknownAttr { kind, name: name.to_owned() });
    };
    let got = value.value_type();
    if got != attr.ty {
        return Err(SchemaError::Type { kind, name: name.to_owned(), expected: attr.ty, got });
    }
    if let (false, Value::Str(text)) = (attr.values.is_empty(), value)
        && !attr.values.contains(&text.as_str())
    {
        return Err(SchemaError::EnumValue { kind, name: name.to_owned(), value: text.clone() });
    }
    Ok(())
}
