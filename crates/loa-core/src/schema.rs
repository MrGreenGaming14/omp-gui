//! Element schema v1, declared once in [`schema!`]: kinds, attribute names,
//! allowed parents and attribute types. [`Dom::check`](crate::Dom::check)
//! enforces it on every `insert` and `set`.
//!
//! Extension rules, so adapters never need a schema change to keep data:
//! - any element accepts attributes whose name starts with `x-`, of any type
//! - `<ext>` accepts any attribute and may sit under any element

use crate::value::{Value, ValueType};
use serde::{Deserialize, Serialize};

/// Version of the element schema. `<session schema="…">` records it.
pub const SCHEMA_VERSION: u32 = 1;

/// Reserved attribute name: a `set` on it replaces (or clears) the node text.
pub const TEXT: &str = "#text";

/// Prefix of attributes that bypass the schema.
pub const EXTENSION_PREFIX: &str = "x-";

/// Status and kind enums: wire strings via `loa_util::str_enum!`, stored as `Value::Str`.
macro_rules! value_enums {
    ($($name:ident { $($variant:ident => $text:literal),+ $(,)? })*) => {$(
        loa_util::str_enum! { pub $name { $($variant => $text),+ } }
        impl From<$name> for Value {
            fn from(v: $name) -> Self { Value::Str(v.as_str().to_owned()) }
        }
    )*};
}

value_enums! {
    Role { User => "user", Assistant => "assistant", System => "system" }
    MessageKind { Text => "text", Thinking => "thinking" }
    MessageStatus { Streaming => "streaming", Done => "done", Error => "error", Cancelled => "cancelled" }
    TurnStatus { Running => "running", Waiting => "waiting", Done => "done", Error => "error", Cancelled => "cancelled" }
    CallStatus { Pending => "pending", Running => "running", Done => "done", Error => "error", Cancelled => "cancelled" }
    DecisionKind { Approval => "approval", Question => "question", Plan => "plan", Form => "form" }
    DecisionStatus { Waiting => "waiting", Answered => "answered", Expired => "expired", Cancelled => "cancelled" }
    JobKind { Shell => "shell", Subagent => "subagent" }
    JobStatus { Running => "running", Done => "done", Error => "error", Cancelled => "cancelled" }
    TodoState { Pending => "pending", InProgress => "in_progress", Done => "done", Cancelled => "cancelled" }
    SurfaceMode { Pixel => "pixel", Document => "document" }
    SurfaceStatus { Open => "open", Closed => "closed" }
    SessionMode { Normal => "normal", Plan => "plan" }
}

/// Emits the `attr` name constants, [`ElementKind`] and one [`ElementSpec`] per
/// kind. Attributes read `req|opt NAME: Type` or `opt NAME: Str(Enum)`.
macro_rules! schema {
    (
        attrs { $($attr:ident = $attr_name:literal),* $(,)? }
        $($kind:ident = $tag:literal under $parents:tt $(, open = $open:literal)? {
            $($q:ident $name:ident : $ty:ident $(($enum:ident))?),* $(,)?
        })*
    ) => {
        /// Attribute names of schema v1.
        pub mod attr { $(pub const $attr: &str = $attr_name;)* }

        /// Every element of schema v1. `as_str` is the tag name.
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        pub enum ElementKind { $($kind),* }

        impl ElementKind {
            pub const ALL: &'static [ElementKind] = &[$(ElementKind::$kind),*];
            pub const VALUES: &'static [&'static str] = &[$($tag),*];

            pub const fn as_str(self) -> &'static str {
                match self { $(ElementKind::$kind => $tag),* }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text { $($tag => Some(ElementKind::$kind),)* _ => None }
            }

            pub fn spec(self) -> &'static ElementSpec {
                #[allow(unused_imports)]
                use ElementKind::*;
                match self {$(
                    $kind => {
                        static SPEC: ElementSpec = ElementSpec {
                            kind: $kind,
                            parents: schema!(@parents $parents),
                            attrs: &[$(AttrSpec {
                                name: attr::$name,
                                ty: ValueType::$ty,
                                required: schema!(@req $q),
                                values: { let _v: &[&str] = &[]; $(let _v = $enum::VALUES;)? _v },
                            }),*],
                            open: { let _o = false; $(let _o = $open;)? _o },
                        };
                        &SPEC
                    }
                )*}
            }
        }
    };
    (@parents *) => { ElementKind::ALL };
    (@parents [$($p:ident),*]) => { &[$($p),*] };
    (@req req) => { true };
    (@req opt) => { false };
}

schema! {
    attrs {
        ID = "id", SCHEMA = "schema", HARNESS = "harness", PROJECT = "project", NODE = "node",
        CREATED = "created", TITLE = "title", CWD = "cwd", MODEL = "model", ROLE = "role",
        MODEL_ROLE = "model_role", EFFORT = "effort", MODE = "mode", PARENT_SESSION = "parent_session",
        FORKED_AT = "forked_at", NATIVE_ID = "native_id", EXCLUDED = "excluded", STATUS = "status",
        STARTED = "started", ENDED = "ended", KIND = "kind", TOOL = "tool", CALL_ID = "call_id",
        FORMAT = "format", IS_ERROR = "is_error", TRUNCATED = "truncated", INPUT_TOKENS = "input_tokens",
        OUTPUT_TOKENS = "output_tokens", CACHE_READ_TOKENS = "cache_read_tokens",
        CACHE_WRITE_TOKENS = "cache_write_tokens", COST = "cost", DURATION_MS = "duration_ms",
        OPTIONS = "options", MULTI = "multi", ANSWER = "answer", BY = "by", REASON = "reason",
        CALL = "call", DEFAULT = "default", TIMEOUT_MS = "timeout_ms", REQUEST_ID = "request_id",
        FIELDS = "fields", COMMAND = "command", SESSION = "session", AGENT = "agent",
        EXIT_CODE = "exit_code", STATE = "state", PHASE = "phase", URI = "uri", NAME = "name",
        WIDTH = "width", HEIGHT = "height", SERVER = "server", COVERS = "covers",
        TOKENS_BEFORE = "tokens_before", SOURCE_SEQ = "source_seq", STALE = "stale",
        DECISIONS = "decisions", FILES = "files", OPEN_ITEMS = "open_items", POINTERS = "pointers",
        TYPE = "type", RAW = "raw",
    }
    // `node` is the owner's public key; `created` is unix ms; `parent_session`
    // and `forked_at` record fork lineage; `native_id` is the harness's own id.
    Session = "session" under [] {
        req ID: Str, req HARNESS: Str, req NODE: Str, req CREATED: Int, opt SCHEMA: Int, opt PROJECT: Str,
        opt TITLE: Str, opt CWD: Str, opt MODEL: Str, opt MODEL_ROLE: Str, opt EFFORT: Str,
        opt MODE: Str(SessionMode), opt PARENT_SESSION: Str, opt FORKED_AT: Int, opt NATIVE_ID: Str,
        opt EXCLUDED: Bool,
    }
    Turn = "turn" under [Session] {
        opt ID: Str, opt STATUS: Str(TurnStatus), opt STARTED: Int, opt ENDED: Int,
    }
    // An absent `kind` means "text".
    Message = "message" under [Turn] {
        req ROLE: Str(Role), opt KIND: Str(MessageKind), opt ID: Str, opt MODEL: Str,
        opt STATUS: Str(MessageStatus), opt BY: Str,
    }
    Call = "call" under [Turn] {
        req TOOL: Str, opt STATUS: Str(CallStatus), opt CALL_ID: Str, opt TITLE: Str, opt STARTED: Int,
        opt ENDED: Int,
    }
    // Text holds the arguments; `format` is "json" unless stated.
    Input = "input" under [Call] { opt FORMAT: Str }
    Result = "result" under [Call] { opt FORMAT: Str, opt IS_ERROR: Bool, opt TRUNCATED: Bool }
    Usage = "usage" under [Call, Message, Turn, Job] {
        opt INPUT_TOKENS: Int, opt OUTPUT_TOKENS: Int, opt CACHE_READ_TOKENS: Int, opt CACHE_WRITE_TOKENS: Int,
        opt COST: Float, opt DURATION_MS: Int, opt MODEL: Str,
    }
    // Text holds the body. `answer` holds picked options, typed text or an
    // [`answer`] value; `call` is the node id of the approved `<call>`;
    // `default` applies when `timeout_ms` passes; `fields` is a JSON Schema.
    Decision = "decision" under [Turn, Call, Session] {
        req KIND: Str(DecisionKind), opt STATUS: Str(DecisionStatus), opt TITLE: Str, opt OPTIONS: List,
        opt MULTI: Bool, opt ANSWER: List, opt BY: Str, opt REASON: Str, opt CALL: Int, opt DEFAULT: List,
        opt TIMEOUT_MS: Int, opt REQUEST_ID: Str, opt FIELDS: Str, opt CREATED: Int, opt ENDED: Int,
    }
    // Text holds output so far; `session` links a subagent's child session.
    Job = "job" under [Turn, Call, Session] {
        req KIND: Str(JobKind), opt STATUS: Str(JobStatus), opt ID: Str, opt TITLE: Str, opt COMMAND: Str,
        opt SESSION: Str, opt AGENT: Str, opt EXIT_CODE: Int, opt CALL: Int, opt STARTED: Int, opt ENDED: Int,
    }
    Todo = "todo" under [Turn, Session] { opt STATE: Str(TodoState), opt ID: Str, opt PHASE: Str }
    Surface = "surface" under [Turn, Call, Session] {
        req URI: Str, req MODE: Str(SurfaceMode), opt NAME: Str, opt WIDTH: Int, opt HEIGHT: Int,
        opt SERVER: Str, opt STATUS: Str(SurfaceStatus),
    }
    // Text holds the summary; `covers` lists the replaced turns' node ids.
    Compaction = "compaction" under [Session] {
        opt COVERS: List, opt TOKENS_BEFORE: Int, opt MODEL: Str, opt CREATED: Int,
    }
    // `source_seq` is the patch it was built from; `pointers` are turn node ids.
    Digest = "digest" under [Session] {
        req SOURCE_SEQ: Int, opt STALE: Bool, opt DECISIONS: List, opt FILES: List, opt OPEN_ITEMS: List,
        opt POINTERS: List, opt MODEL: Str, opt CREATED: Int,
    }
    // `raw` or the text keeps the harness's own data so round trips are lossless.
    Ext = "ext" under *, open = true { req HARNESS: Str, opt TYPE: Str }
}

impl From<ElementKind> for Value {
    fn from(v: ElementKind) -> Self {
        Value::Str(v.as_str().to_owned())
    }
}

impl std::fmt::Display for ElementKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical `answer` values written on `<decision>` elements.
pub mod answer {
    /// Approval: run once, allow for the session, or refuse (`reason` says why).
    pub const ALLOW_ONCE: &str = "allow_once";
    pub const ALLOW_SESSION: &str = "allow_session";
    pub const DENY: &str = "deny";
    /// Plan review: run as written, run the edited plan in the decision text,
    /// or refuse with feedback in `reason`.
    pub const APPROVE: &str = "approve";
    pub const EDIT: &str = "edit";
    pub const REJECT: &str = "reject";
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

impl ElementSpec {
    pub fn attr(&self, name: &str) -> Option<&'static AttrSpec> {
        self.attrs.iter().find(|a| a.name == name)
    }

    fn unlisted_ok(&self, name: &str) -> bool {
        self.open || name.starts_with(EXTENSION_PREFIX)
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
    let parent_ok = parent.map_or(spec.parents.is_empty(), |p| spec.parents.contains(&p));
    if !parent_ok {
        return Err(SchemaError::Parent { child: kind, parent });
    }
    for (name, value) in attrs.clone() {
        validate_attr(spec, name, value)?;
    }
    match spec.attrs.iter().find(|a| a.required && !attrs.clone().into_iter().any(|(n, _)| n == a.name)) {
        Some(missing) => Err(SchemaError::Required { kind, name: missing.name }),
        None => Ok(()),
    }
}

/// Checks a `set` of `name` on an element of `kind`. `None` clears.
pub fn validate_set(kind: ElementKind, name: &str, value: Option<&Value>) -> Result<(), SchemaError> {
    let spec = kind.spec();
    match (name == TEXT, value) {
        (true, None | Some(Value::Str(_))) => Ok(()),
        (true, Some(_)) => Err(SchemaError::TextType),
        (false, Some(value)) => validate_attr(spec, name, value),
        (false, None) => match spec.attr(name) {
            Some(a) if a.required => Err(SchemaError::Required { kind, name: a.name }),
            Some(_) => Ok(()),
            None if spec.unlisted_ok(name) => Ok(()),
            None => Err(SchemaError::UnknownAttr { kind, name: name.to_owned() }),
        },
    }
}

fn validate_attr(spec: &ElementSpec, name: &str, value: &Value) -> Result<(), SchemaError> {
    let kind = spec.kind;
    let Some(attr) = spec.attr(name) else {
        return match spec.unlisted_ok(name) {
            true => Ok(()),
            false => Err(SchemaError::UnknownAttr { kind, name: name.to_owned() }),
        };
    };
    let got = value.value_type();
    if got != attr.ty {
        return Err(SchemaError::Type { kind, name: name.to_owned(), expected: attr.ty, got });
    }
    match value {
        Value::Str(text) if !attr.values.is_empty() && !attr.values.contains(&text.as_str()) => {
            Err(SchemaError::EnumValue { kind, name: name.to_owned(), value: text.clone() })
        }
        _ => Ok(()),
    }
}
