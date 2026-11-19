//! The patch@1 to schema v1 translation table: which omp2 element becomes
//! which app element, and which omp2 property becomes which attribute.
//!
//! Pure data. [`crate::translate`] applies it; anything the table doesn't
//! name lands in an `x-` attribute or an `<ext>` element.

use crate::wire::{ASSISTANT_CONTENT_TAG, Tag};
use loa_core::{ElementKind, attr};

/// Attribute holding the omp2 handle of the node an app node came from. It
/// makes the handle table recoverable from a loaded DOM on resume.
pub const HANDLE_ATTR: &str = "x-omp2-handle";

/// omp2's `order` prop is a journal entry id rewritten on every stream
/// append. It only orders entries inside the omp2 journal, so it is dropped
/// instead of doubling the app journal with one `set` per token.
pub const DROPPED_PROPS: &[&str] = &["order"];

/// What an omp2 element becomes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The omp2 `<session>` root: the app `<session>` the adapter inserted.
    Root,
    /// A pure container (`<meta>`, `<body>`, `<todo>`, `<jobs>`). Its
    /// children go under the app node of its parent; its own props become
    /// `x-<tag>-<prop>` attributes there.
    Flatten,
    /// `<assistant>`: a container of content blocks. Flattened into the turn;
    /// each block becomes one `<message role="assistant">` that inherits the
    /// assistant's model.
    Assistant,
    Element(Element),
    /// No shared element: `<ext harness="omp2" type="<tag>">` with every prop
    /// kept as an attribute and the content as text. Its subtree stays `<ext>`.
    Ext,
}

/// The omp2 elements that have an app counterpart.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Element {
    Turn,
    User,
    /// One `assistant-content` block.
    Block,
    /// A tool call: any custom tag directly under a turn.
    Call,
    Input,
    Result,
    Usage,
    /// A todo `<item>`.
    Todo,
    /// A background `<job>`.
    Job,
    Subagent,
    Compaction,
}

impl Element {
    pub fn kind(self) -> ElementKind {
        match self {
            Element::Turn => ElementKind::Turn,
            Element::User | Element::Block => ElementKind::Message,
            Element::Call => ElementKind::Call,
            Element::Input => ElementKind::Input,
            Element::Result => ElementKind::Result,
            Element::Usage => ElementKind::Usage,
            Element::Todo => ElementKind::Todo,
            Element::Job | Element::Subagent => ElementKind::Job,
            Element::Compaction => ElementKind::Compaction,
        }
    }
}

/// The rule for `tag` inserted under the omp2 element `parent_tag`, whose
/// nearest app node has kind `parent` (`None` is an `<ext>`: the subtree
/// stays raw). A result the schema refuses under `parent` falls back to
/// [`Rule::Ext`] when applied.
pub fn rule(tag: &Tag, parent_tag: &Tag, parent: Option<ElementKind>) -> Rule {
    let Some(parent) = parent else { return Rule::Ext };
    if tag.custom {
        return match (tag.name.as_str(), parent) {
            (ASSISTANT_CONTENT_TAG, ElementKind::Turn) if parent_tag.is("assistant") => Rule::Element(Element::Block),
            (_, ElementKind::Turn) if parent_tag.is("turn") => Rule::Element(Element::Call),
            _ => Rule::Ext,
        };
    }
    match tag.name.as_str() {
        "session" => Rule::Root,
        "meta" | "body" | "todo" | "jobs" => Rule::Flatten,
        "assistant" => Rule::Assistant,
        "turn" => Rule::Element(Element::Turn),
        "user" => Rule::Element(Element::User),
        "item" => Rule::Element(Element::Todo),
        "job" => Rule::Element(Element::Job),
        "subagent" => Rule::Element(Element::Subagent),
        "compaction" => Rule::Element(Element::Compaction),
        "input" => Rule::Element(Element::Input),
        "result" => Rule::Element(Element::Result),
        "usage" => Rule::Element(Element::Usage),
        // queues, steering, prompts, prompt, directors, director, con, var,
        // developer, notice, diag
        _ => Rule::Ext,
    }
}

/// Where one omp2 property goes on an app element.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dest {
    /// A schema attribute. Enum attributes pass through [`enum_value`].
    Attr(&'static str),
    /// The node text.
    Text,
    /// `cost-nano-usd` to `cost` in US dollars.
    Cost,
    /// Not kept, see [`DROPPED_PROPS`].
    Drop,
    /// `x-<prop>`.
    Extra,
}

/// Where `prop` of an omp2 node that became `element` goes.
pub fn dest(element: Element, prop: &str) -> Dest {
    if DROPPED_PROPS.contains(&prop) {
        return Dest::Drop;
    }
    use Element as E;
    match (element, prop) {
        (E::Turn, "id") => Dest::Attr(attr::ID),
        (E::User, "id") => Dest::Attr(attr::ID),
        (E::User, "author") => Dest::Attr(attr::BY),
        (E::User | E::Block | E::Input | E::Result, "text") => Dest::Text,
        (E::Block, "kind") => Dest::Attr(attr::KIND),
        (E::Call, "id") => Dest::Attr(attr::CALL_ID),
        (E::Call, "status") => Dest::Attr(attr::STATUS),
        (E::Call, "i") => Dest::Attr(attr::TITLE),
        (E::Result, "truncated") => Dest::Attr(attr::TRUNCATED),
        (E::Usage, "tokens-in") => Dest::Attr(attr::INPUT_TOKENS),
        (E::Usage, "tokens-out") => Dest::Attr(attr::OUTPUT_TOKENS),
        (E::Usage, "cache-read") => Dest::Attr(attr::CACHE_READ_TOKENS),
        (E::Usage, "cache-write") => Dest::Attr(attr::CACHE_WRITE_TOKENS),
        (E::Usage, "cost-nano-usd") => Dest::Cost,
        (E::Usage, "duration-ms") => Dest::Attr(attr::DURATION_MS),
        (E::Usage, "model") => Dest::Attr(attr::MODEL),
        (E::Todo, "label") => Dest::Text,
        (E::Todo, "status") => Dest::Attr(attr::STATE),
        (E::Todo, "phase") => Dest::Attr(attr::PHASE),
        (E::Job | E::Subagent, "id") => Dest::Attr(attr::ID),
        (E::Job | E::Subagent, "status") => Dest::Attr(attr::STATUS),
        (E::Job | E::Subagent, "label") => Dest::Attr(attr::TITLE),
        (E::Job | E::Subagent, "agent") => Dest::Attr(attr::AGENT),
        (E::Job | E::Subagent, "exit") => Dest::Attr(attr::EXIT_CODE),
        (E::Compaction, "summary") => Dest::Text,
        (E::Compaction, "tokens-before") => Dest::Attr(attr::TOKENS_BEFORE),
        _ => Dest::Extra,
    }
}

/// The schema value for an omp2 enum value, `None` when it has no
/// counterpart (the prop is then kept as `x-<prop>`).
pub fn enum_value(element: Element, name: &str, raw: &str) -> Option<&'static str> {
    use Element as E;
    match (element, name) {
        (E::Call, attr::STATUS) => match raw {
            "arguments" => Some("pending"),
            "running" => Some("running"),
            "ok" => Some("done"),
            "error" => Some("error"),
            "cancelled" | "aborted" => Some("cancelled"),
            _ => None,
        },
        (E::Todo, attr::STATE) => match raw {
            "pending" | "blocked" => Some("pending"),
            "in_progress" => Some("in_progress"),
            "completed" => Some("done"),
            "abandoned" => Some("cancelled"),
            _ => None,
        },
        (E::Job | E::Subagent, attr::STATUS) => match raw {
            "running" => Some("running"),
            "completed" | "done" | "ok" => Some("done"),
            "failed" | "error" => Some("error"),
            "cancelled" | "killed" | "aborted" => Some("cancelled"),
            _ => None,
        },
        (E::Block, attr::KIND) => match raw {
            "text" => Some("text"),
            "thinking" => Some("thinking"),
            _ => None,
        },
        _ => None,
    }
}

/// Attributes every new app element of `element` starts with, before its
/// mapped props.
pub fn fixed_attrs(element: Element, tag: &Tag) -> Vec<(&'static str, loa_core::Value)> {
    use Element as E;
    use loa_core::Value;
    match element {
        E::Turn => vec![(attr::STATUS, Value::from("running"))],
        E::User => vec![(attr::ROLE, Value::from("user")), (attr::STATUS, Value::from("done"))],
        E::Block => vec![(attr::ROLE, Value::from("assistant")), (attr::STATUS, Value::from("streaming"))],
        E::Call => vec![(attr::TOOL, Value::Str(tag.name.clone())), (attr::STATUS, Value::from("running"))],
        E::Input => vec![(attr::FORMAT, Value::from("json"))],
        E::Job => vec![(attr::KIND, Value::from("shell"))],
        E::Subagent => vec![(attr::KIND, Value::from("subagent"))],
        E::Result | E::Usage | E::Todo | E::Compaction => Vec::new(),
    }
}

/// The schema attribute of a prop on the omp2 `<session>` root.
pub fn session_dest(prop: &str) -> Dest {
    match prop {
        "name" => Dest::Attr(attr::TITLE),
        _ => Dest::Extra,
    }
}

/// The final `<message status>` of an assistant block given the
/// assistant's `stop-reason`.
pub fn stop_status(reason: &str) -> &'static str {
    match reason {
        "aborted" | "cancelled" => "cancelled",
        "error" => "error",
        _ => "done",
    }
}
