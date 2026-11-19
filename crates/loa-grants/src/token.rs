//! Grant tokens: building, attenuating, verifying and authorizing.

use crate::keys::{Challenge, NodeKey, NodePublicKey, Proof, proof_valid};
use crate::scope::{Action, Request, Resource, Scope, Target};
use crate::{GrantError, Result};
use biscuit_auth::builder::{Term, fact, int, set, string};
use biscuit_auth::error::{FailedCheck, Logic, Token as BiscuitError};
use biscuit_auth::{AuthorizerBuilder, AuthorizerLimits, Biscuit, BiscuitBuilder, BlockBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

/// Id of one grant definition. Stays the same across renewals.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantId(pub String);

impl GrantId {
    pub(crate) fn random() -> Result<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| GrantError::Random(e.to_string()))?;
        Ok(Self(hex::encode(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GrantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What each block's signed context records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BlockContext {
    #[serde(rename = "g")]
    grant: GrantId,
    #[serde(rename = "h")]
    holder: String,
    #[serde(rename = "e")]
    expires_ms: i64,
    #[serde(rename = "r")]
    reshare: bool,
}

/// One holder in a token's chain: the original grant (index 0) or a re-share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub grant: GrantId,
    /// Who this block was issued to.
    pub holder: NodePublicKey,
    /// Who signed this block: the root node for index 0, the previous holder
    /// for a re-share.
    pub issuer: NodePublicKey,
    pub expires_ms: i64,
    /// Whether `holder` may re-share.
    pub reshare: bool,
}

/// A token parsed and its chain checked, without a holder proof yet.
#[derive(Clone, Debug)]
pub(crate) struct Chain {
    pub biscuit: Biscuit,
    pub links: Vec<Link>,
}

impl Chain {
    pub fn last(&self) -> &Link {
        // `parse` never builds a chain without the authority link.
        &self.links[self.links.len() - 1]
    }

    pub fn expires_ms(&self) -> i64 {
        self.links.iter().map(|l| l.expires_ms).min().unwrap_or(i64::MIN)
    }

    /// Verifies the signatures against `root` and walks the holder chain.
    pub fn parse(token: &[u8], root: &NodePublicKey) -> Result<Self, Denied> {
        let biscuit = Biscuit::from(token, root.biscuit().map_err(|e| Denied::BadToken(e.to_string()))?)
            .map_err(|e| Denied::BadToken(e.to_string()))?;
        let contexts = biscuit.context();
        let signers = biscuit.external_public_keys();
        let mut links: Vec<Link> = Vec::with_capacity(contexts.len());
        for (index, (context, signer)) in contexts.iter().zip(&signers).enumerate() {
            let context: BlockContext = context
                .as_deref()
                .and_then(|c| serde_json::from_str(c).ok())
                .ok_or(Denied::BrokenChain { block: index })?;
            let holder = NodePublicKey::from_hex(&context.holder).map_err(|_| Denied::BrokenChain { block: index })?;
            let issuer = match links.last() {
                None => *root,
                Some(previous) => {
                    let signer = signer
                        .as_ref()
                        .and_then(NodePublicKey::from_biscuit)
                        .ok_or(Denied::BrokenChain { block: index })?;
                    if signer != previous.holder {
                        return Err(Denied::BrokenChain { block: index });
                    }
                    if !previous.reshare {
                        return Err(Denied::ReshareForbidden { block: index });
                    }
                    signer
                }
            };
            links.push(Link {
                grant: context.grant,
                holder,
                issuer,
                expires_ms: context.expires_ms,
                reshare: context.reshare,
            });
        }
        if links.is_empty() {
            return Err(Denied::BrokenChain { block: 0 });
        }
        Ok(Self { biscuit, links })
    }
}

fn right_fact(scope: &Scope, action: Action) -> biscuit_auth::builder::Fact {
    let a = string(action.as_str());
    match &scope.resource {
        Resource::Session { id } => fact("session_right", &[string(id), a]),
        // `Scope::new` checked the id fits an i64.
        Resource::Prompt { session, turn } => fact("prompt_right", &[string(session), int(turn.0 as i64), a]),
        Resource::Project { id } => fact("project_right", &[string(id), a]),
        Resource::FutureSessions { after_ms } => fact("future_right", &[int(*after_ms), a]),
        Resource::Ssh { host } => fact("ssh_right", &[string(host), a]),
        Resource::Surface { uri } => fact("surface_right", &[string(uri), a]),
        Resource::Sandbox => fact("sandbox_right", &[string(SANDBOX), a]),
        Resource::Folder { path } => fact("folder_right", &[string(path), a]),
    }
}

/// Builds the original token: the scopes as facts, an expiry check, and the
/// chain context, signed by `issuer`.
pub(crate) fn mint(
    issuer: &NodeKey,
    grant: &GrantId,
    holder: &NodePublicKey,
    scopes: &[Scope],
    expires_ms: i64,
    reshare: bool,
) -> Result<Vec<u8>> {
    let mut builder = BiscuitBuilder::new();
    for scope in scopes {
        for &action in &scope.actions {
            builder = builder.fact(right_fact(scope, action))?;
        }
    }
    builder = builder.code_with_params(EXPIRY_CHECK, expiry_param(expires_ms), HashMap::new())?;
    builder = builder.context(context_json(grant, holder, expires_ms, reshare)?);
    Ok(builder.build(issuer.pair())?.to_vec()?)
}

/// Appends a re-share block signed by `holder` (the current last holder).
pub(crate) fn attenuate(
    parent: &Chain,
    holder: &NodeKey,
    grant: &GrantId,
    new_holder: &NodePublicKey,
    scopes: Option<&[Scope]>,
    expires_ms: i64,
    reshare: bool,
) -> Result<Vec<u8>> {
    let mut block = BlockBuilder::new().code_with_params(EXPIRY_CHECK, expiry_param(expires_ms), HashMap::new())?;
    if let Some(scopes) = scopes {
        let (source, params) = narrowing_check(scopes);
        block = block.code_with_params(source, params, HashMap::new())?;
    }
    block = block.context(context_json(grant, new_holder, expires_ms, reshare)?);
    let request = parent.biscuit.third_party_request()?;
    let signed = request.create_block(&holder.private(), block)?;
    let token = parent.biscuit.append_third_party(holder.pair().public(), signed)?;
    Ok(token.to_vec()?)
}

/// The rights a token's issuer signed into its first block, as scopes. Later
/// re-share blocks can only narrow these; they are not reflected here. For
/// showing a holder what it was given; authorization goes through [`Verifier`].
pub fn token_scopes(token: &[u8], root: &NodePublicKey) -> Result<Vec<Scope>> {
    let biscuit = Biscuit::from(token, root.biscuit()?)?;
    let mut authorizer = AuthorizerBuilder::new().build(&biscuit)?;
    let mut scopes: Vec<Scope> = Vec::new();
    let mut add = |resource: Resource, action: &str| -> Result<()> {
        let action = Action::parse(action).ok_or_else(|| GrantError::Malformed(format!("unknown action {action:?}")))?;
        match scopes.iter_mut().find(|s| s.resource == resource) {
            Some(scope) => {
                scope.actions.insert(action);
            }
            None => scopes.push(Scope { resource, actions: BTreeSet::from([action]) }),
        }
        Ok(())
    };
    let sessions: Vec<(String, String)> = authorizer.query("r($s, $a) <- session_right($s, $a)")?;
    for (id, action) in sessions {
        add(Resource::Session { id }, &action)?;
    }
    let prompts: Vec<(String, i64, String)> = authorizer.query("r($s, $t, $a) <- prompt_right($s, $t, $a)")?;
    for (session, turn, action) in prompts {
        let turn = loa_core::NodeId(u64::try_from(turn).map_err(|_| GrantError::Malformed(format!("turn {turn}")))?);
        add(Resource::Prompt { session, turn }, &action)?;
    }
    let projects: Vec<(String, String)> = authorizer.query("r($p, $a) <- project_right($p, $a)")?;
    for (id, action) in projects {
        add(Resource::Project { id }, &action)?;
    }
    let future: Vec<(i64, String)> = authorizer.query("r($t, $a) <- future_right($t, $a)")?;
    for (after_ms, action) in future {
        add(Resource::FutureSessions { after_ms }, &action)?;
    }
    let ssh: Vec<(String, String)> = authorizer.query("r($h, $a) <- ssh_right($h, $a)")?;
    for (host, action) in ssh {
        add(Resource::Ssh { host }, &action)?;
    }
    let surfaces: Vec<(String, String)> = authorizer.query("r($u, $a) <- surface_right($u, $a)")?;
    for (uri, action) in surfaces {
        add(Resource::Surface { uri }, &action)?;
    }
    let sandboxes: Vec<(String, String)> = authorizer.query("r($n, $a) <- sandbox_right($n, $a)")?;
    for (_, action) in sandboxes {
        add(Resource::Sandbox, &action)?;
    }
    let folders: Vec<(String, String)> = authorizer.query("r($p, $a) <- folder_right($p, $a)")?;
    for (path, action) in folders {
        add(Resource::Folder { path }, &action)?;
    }
    Ok(scopes)
}

const EXPIRY_CHECK: &str = "check if time_ms($t), $t < {expires};";

fn expiry_param(expires_ms: i64) -> HashMap<String, Term> {
    HashMap::from([("expires".to_owned(), int(expires_ms))])
}

fn context_json(grant: &GrantId, holder: &NodePublicKey, expires_ms: i64, reshare: bool) -> Result<String> {
    Ok(serde_json::to_string(&BlockContext { grant: grant.clone(), holder: holder.to_hex(), expires_ms, reshare })?)
}

/// One check that passes when the request falls in any of `scopes`. All
/// values go in as parameters, so names can't inject Datalog.
fn narrowing_check(scopes: &[Scope]) -> (String, HashMap<String, Term>) {
    let mut params = HashMap::new();
    let mut bodies = Vec::with_capacity(scopes.len());
    for (i, scope) in scopes.iter().enumerate() {
        let actions: BTreeSet<Term> = scope.actions.iter().map(|a| string(a.as_str())).collect();
        params.insert(format!("a{i}"), set(actions));
        let (body, values): (String, Vec<Term>) = match &scope.resource {
            Resource::Session { id } => (format!("session({{v{i}x0}})"), vec![string(id)]),
            Resource::Prompt { session, turn } => {
                (format!("session({{v{i}x0}}), turn({{v{i}x1}})"), vec![string(session), int(turn.0 as i64)])
            }
            Resource::Project { id } => (format!("project({{v{i}x0}})"), vec![string(id)]),
            Resource::FutureSessions { after_ms } => (format!("created($c), $c >= {{v{i}x0}}"), vec![int(*after_ms)]),
            Resource::Ssh { host } => (format!("ssh_host({{v{i}x0}})"), vec![string(host)]),
            Resource::Surface { uri } => (format!("surface({{v{i}x0}})"), vec![string(uri)]),
            Resource::Sandbox => (format!("sandbox({{v{i}x0}})"), vec![string(SANDBOX)]),
            Resource::Folder { path } => (format!("folder({{v{i}x0}})"), vec![string(path)]),
        };
        for (j, value) in values.into_iter().enumerate() {
            params.insert(format!("v{i}x{j}"), value);
        }
        bodies.push(format!("action($a), {body}, {{a{i}}}.contains($a)"));
    }
    if bodies.is_empty() {
        // Narrowed to nothing: a check that can never pass.
        return ("check if false;".to_owned(), params);
    }
    (format!("check if {};", bodies.join(" or ")), params)
}

/// The one value the sandbox facts carry: the issuing node's sandbox.
const SANDBOX: &str = "node";

/// The verifier's rules: which request facts a right covers.
const RULES: &str = r#"
granted($a) <- session_right($s, $a), session($s);
granted($a) <- prompt_right($s, $t, $a), session($s), turn($t);
granted($a) <- project_right($p, $a), project($p);
granted($a) <- future_right($after, $a), created($c), $c >= $after;
granted($a) <- ssh_right($h, $a), ssh_host($h);
granted($a) <- surface_right($u, $a), surface($u);
granted($a) <- sandbox_right($n, $a), sandbox($n);
granted($a) <- folder_right($p, $a), folder($p);
allow if action($a), granted($a);
"#;

/// Grant ids this node knows to be revoked.
pub trait Revocations {
    fn is_revoked(&self, grant: &GrantId) -> bool;
}

impl<R: Revocations + ?Sized> Revocations for &R {
    fn is_revoked(&self, grant: &GrantId) -> bool {
        (**self).is_revoked(grant)
    }
}

impl<R: Revocations + ?Sized> Revocations for std::sync::Arc<R> {
    fn is_revoked(&self, grant: &GrantId) -> bool {
        (**self).is_revoked(grant)
    }
}

/// Why a token or a request was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("the token doesn't parse or isn't signed by this node: {0}")]
    BadToken(String),
    #[error("block {block} isn't signed by the holder before it")]
    BrokenChain { block: usize },
    #[error("block {block} re-shares a grant that may not be re-shared")]
    ReshareForbidden { block: usize },
    #[error("the token was issued to {holder}, not to {requester}")]
    NotHolder { holder: NodePublicKey, requester: NodePublicKey },
    #[error("the holder's proof doesn't match the challenge")]
    BadProof,
    #[error("grant {0} is revoked")]
    Revoked(GrantId),
    #[error("the token expired at {expired_ms}")]
    Expired { expired_ms: i64 },
    #[error("the grant has no `{action}` right on this {resource}")]
    NotGranted { action: Action, resource: &'static str },
    #[error("re-share {block} narrowed `{action}` on this {resource} away")]
    Narrowed { action: Action, resource: &'static str, block: usize },
    #[error("authorization failed: {0}")]
    Engine(String),
}

/// A token presented by a peer, with its proof over the verifier's challenge.
#[derive(Clone, Copy, Debug)]
pub struct Presented<'a> {
    pub token: &'a [u8],
    pub proof: &'a Proof,
}

/// Checks tokens presented to the node that issued them (the root). Holds the
/// node's public key and its revocation list.
pub struct Verifier<R> {
    root: NodePublicKey,
    revocations: R,
}

impl<R: Revocations> Verifier<R> {
    pub fn new(root: NodePublicKey, revocations: R) -> Self {
        Self { root, revocations }
    }

    /// Checks signatures, the holder chain, that `requester` is the last
    /// holder and signed `challenge`, revocation and expiry. Do this once
    /// per connection, then call [`VerifiedGrant::authorize`] per request.
    pub fn verify(
        &self,
        presented: Presented<'_>,
        requester: &NodePublicKey,
        challenge: &Challenge,
        now_ms: i64,
    ) -> Result<VerifiedGrant, Denied> {
        let chain = Chain::parse(presented.token, &self.root)?;
        let holder = chain.last().holder;
        if holder != *requester {
            return Err(Denied::NotHolder { holder, requester: *requester });
        }
        if !proof_valid(requester, challenge, presented.token, presented.proof) {
            return Err(Denied::BadProof);
        }
        if let Some(link) = chain.links.iter().find(|l| self.revocations.is_revoked(&l.grant)) {
            return Err(Denied::Revoked(link.grant.clone()));
        }
        let grant = VerifiedGrant { chain };
        grant.live(now_ms)?;
        Ok(grant)
    }

    /// [`Verifier::verify`] and [`VerifiedGrant::authorize`] in one call.
    pub fn authorize(
        &self,
        presented: Presented<'_>,
        requester: &NodePublicKey,
        challenge: &Challenge,
        request: &Request<'_>,
        now_ms: i64,
    ) -> Result<(), Denied> {
        self.verify(presented, requester, challenge, now_ms)?.authorize(request, now_ms)
    }
}

/// A token whose holder proved possession. Answers requests until it expires.
#[derive(Clone, Debug)]
pub struct VerifiedGrant {
    chain: Chain,
}

impl VerifiedGrant {
    /// The holders from the original grant to the presenter.
    pub fn chain(&self) -> &[Link] {
        &self.chain.links
    }

    pub fn holder(&self) -> NodePublicKey {
        self.chain.last().holder
    }

    /// When the token stops working: the earliest expiry in the chain.
    pub fn expires_ms(&self) -> i64 {
        self.chain.expires_ms()
    }

    fn live(&self, now_ms: i64) -> Result<(), Denied> {
        let expired_ms = self.expires_ms();
        if now_ms >= expired_ms {
            return Err(Denied::Expired { expired_ms });
        }
        Ok(())
    }

    /// Allows or denies `request` at `now_ms`.
    pub fn authorize(&self, request: &Request<'_>, now_ms: i64) -> Result<(), Denied> {
        self.live(now_ms)?;
        let resource = match request.target {
            Target::Session { turn: Some(_), .. } => "prompt",
            Target::Session { .. } => "session",
            Target::Ssh { .. } => "ssh host",
            Target::Surface { .. } => "surface",
            Target::Sandbox => "sandbox",
            Target::Folder { .. } => "folder",
        };
        let engine = |e: BiscuitError| Denied::Engine(e.to_string());
        let mut builder = AuthorizerBuilder::new()
            .code(RULES)
            .map_err(engine)?
            .set_limits(AuthorizerLimits { max_facts: 1000, max_iterations: 100, max_time: Duration::from_millis(250) });
        let mut facts = vec![fact("action", &[string(request.action.as_str())]), fact("time_ms", &[int(now_ms)])];
        match request.target {
            Target::Session { session, turn } => {
                facts.push(fact("session", &[string(&session.id)]));
                facts.push(fact("created", &[int(session.created_ms)]));
                if let Some(project) = &session.project {
                    facts.push(fact("project", &[string(project)]));
                }
                if let Some(turn) = turn {
                    // Node ids above i64::MAX can't be in any grant (see
                    // `Scope::new`), so such a turn matches nothing.
                    let id = i64::try_from(turn.0).map_err(|_| Denied::NotGranted { action: request.action, resource })?;
                    facts.push(fact("turn", &[int(id)]));
                }
            }
            Target::Ssh { host } => facts.push(fact("ssh_host", &[string(host)])),
            Target::Surface { uri } => facts.push(fact("surface", &[string(uri)])),
            Target::Sandbox => facts.push(fact("sandbox", &[string(SANDBOX)])),
            Target::Folder { path } => {
                // The folder and every folder above it: a grant on any of them covers it.
                let ancestors = crate::scope::folder_ancestors(path)
                    .ok_or(Denied::NotGranted { action: request.action, resource })?;
                facts.extend(ancestors.iter().map(|folder| fact("folder", &[string(folder)])));
            }
        }
        for f in facts {
            builder = builder.fact(f).map_err(engine)?;
        }
        let mut authorizer = builder.build(&self.chain.biscuit).map_err(engine)?;
        match authorizer.authorize() {
            Ok(_) => Ok(()),
            // The allow policy didn't match: the original grant lacks the right.
            Err(BiscuitError::FailedLogic(Logic::NoMatchingPolicy { .. })) => {
                Err(Denied::NotGranted { action: request.action, resource })
            }
            Err(BiscuitError::FailedLogic(Logic::Unauthorized { checks, .. })) => {
                // Expiry was checked above, so a failed check in a re-share
                // block is its narrowing; a failed authority check is expiry.
                let narrowed = checks.iter().find_map(|c| match c {
                    FailedCheck::Block(b) if b.block_id > 0 => Some(b.block_id as usize),
                    _ => None,
                });
                Err(match narrowed {
                    Some(block) => Denied::Narrowed { action: request.action, resource, block },
                    None => Denied::Expired { expired_ms: self.expires_ms() },
                })
            }
            Err(e) => Err(engine(e)),
        }
    }
}
