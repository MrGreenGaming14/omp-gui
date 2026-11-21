//! Grant tokens: minting, attenuating, verifying and authorizing. The Datalog
//! is generated from [`KINDS`].

use crate::key::{Challenge, NodeIdentity, Proof, PublicKey, proof_valid};
use crate::scope::{Action, KINDS, Request, SANDBOX, Scope, Target, folder_ancestors};
use crate::{Result, TrustError};
use biscuit_auth::builder::{Fact, Term, fact, int, set, string};
use biscuit_auth::error::{FailedCheck, Logic, Token as BiscuitError};
use biscuit_auth::{AuthorizerBuilder, AuthorizerLimits, Biscuit, BiscuitBuilder, BlockBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;
use std::time::Duration;

/// Id of one grant definition. Stays the same across renewals.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantId(pub String);

impl GrantId {
    pub(crate) fn random() -> Self { Self(loa_util::text::random_hex(16)) }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for GrantId { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) } }

/// What each block's signed context records.
#[derive(Serialize, Deserialize)]
struct BlockContext {
    g: GrantId,
    /// Holder, as hex.
    h: String,
    e: i64,
    r: bool,
}

/// One holder in a token's chain: the original grant (index 0) or a re-share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub grant: GrantId,
    /// Who this block was issued to.
    pub holder: PublicKey,
    /// Who signed it: the root for index 0, the previous holder for a re-share.
    pub issuer: PublicKey,
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

    pub fn expires_ms(&self) -> i64 { self.links.iter().map(|l| l.expires_ms).min().unwrap_or(i64::MIN) }
    /// Verifies the signatures against `root` and walks the holder chain.
    pub fn parse(token: &[u8], root: &PublicKey) -> Result<Self, Denied> {
        let bad = |e: String| Denied::BadToken(e);
        let key = biscuit_auth::PublicKey::try_from(*root).map_err(|e| bad(e.to_string()))?;
        let biscuit = Biscuit::from(token, key).map_err(|e| bad(e.to_string()))?;
        let mut links: Vec<Link> = Vec::new();
        for (block, (context, signer)) in biscuit.context().iter().zip(biscuit.external_public_keys()).enumerate() {
            let broken = Denied::BrokenChain { block };
            let context: BlockContext = context.as_deref().and_then(|c| serde_json::from_str(c).ok()).ok_or(broken.clone())?;
            let holder = PublicKey::from_hex(&context.h).map_err(|_| broken.clone())?;
            let issuer = match links.last() {
                None => *root,
                Some(previous) => {
                    let signer = signer.as_ref().and_then(|s| PublicKey::try_from(s).ok()).ok_or(broken.clone())?;
                    if signer != previous.holder {
                        return Err(broken);
                    }
                    if !previous.reshare {
                        return Err(Denied::ReshareForbidden { block });
                    }
                    signer
                }
            };
            links.push(Link { grant: context.g, holder, issuer, expires_ms: context.e, reshare: context.r });
        }
        if links.is_empty() {
            return Err(Denied::BrokenChain { block: 0 });
        }
        Ok(Self { biscuit, links })
    }
}

/// Builds the original token: the scopes as right facts, an expiry check and
/// the chain context, signed by `issuer`.
pub(crate) fn mint(issuer: &NodeIdentity, grant: &GrantId, holder: &PublicKey, scopes: &[Scope], expires_ms: i64, reshare: bool) -> Result<Vec<u8>> {
    let mut builder = BiscuitBuilder::new();
    for scope in scopes {
        let (kind, values) = scope.resource.parts();
        for action in &scope.actions {
            let terms: Vec<Term> = values.iter().cloned().chain([string(action.as_str())]).collect();
            builder = builder.fact(fact(kind.right, &terms))?;
        }
    }
    builder = builder.code_with_params(EXPIRY_CHECK, expiry_param(expires_ms), HashMap::new())?;
    builder = builder.context(context_json(grant, holder, expires_ms, reshare)?);
    Ok(builder.build(&issuer.biscuit()?)?.to_vec()?)
}

/// Appends a re-share block signed by `holder`, the chain's current last holder.
#[allow(clippy::too_many_arguments)]
pub(crate) fn attenuate(parent: &Chain, holder: &NodeIdentity, grant: &GrantId, new_holder: &PublicKey, scopes: Option<&[Scope]>, expires_ms: i64, reshare: bool) -> Result<Vec<u8>> {
    let mut block = BlockBuilder::new().code_with_params(EXPIRY_CHECK, expiry_param(expires_ms), HashMap::new())?;
    if let Some(scopes) = scopes {
        let (source, params) = narrowing_check(scopes);
        block = block.code_with_params(source, params, HashMap::new())?;
    }
    block = block.context(context_json(grant, new_holder, expires_ms, reshare)?);
    let pair = holder.biscuit()?;
    let signed = parent.biscuit.third_party_request()?.create_block(&pair.private(), block)?;
    Ok(parent.biscuit.append_third_party(pair.public(), signed)?.to_vec()?)
}

/// The rights a token's root signed into its first block, as scopes. Re-share
/// blocks only narrow these and are not reflected. For display; authorization
/// goes through [`Verifier`].
pub fn token_scopes(token: &[u8], root: &PublicKey) -> Result<Vec<Scope>> {
    let biscuit = Biscuit::from(token, biscuit_auth::PublicKey::try_from(*root)?)?;
    let mut authorizer = AuthorizerBuilder::new().build(&biscuit)?;
    let mut scopes: Vec<Scope> = Vec::new();
    for kind in &KINDS {
        let vars = kind.vars();
        let rights: Vec<Fact> = authorizer.query(format!("r({vars}$a) <- {}({vars}$a)", kind.right).as_str())?;
        for right in rights {
            let malformed = || TrustError::Malformed(format!("bad {} right {right}", kind.name));
            let (action, values) = right.predicate.terms.split_last().ok_or_else(malformed)?;
            let action = if let Term::Str(a) = action { Action::parse(a) } else { None }.ok_or_else(malformed)?;
            let resource = kind.build(values).ok_or_else(malformed)?;
            match scopes.iter_mut().find(|s| s.resource == resource) {
                Some(scope) => _ = scope.actions.insert(action),
                None => scopes.push(Scope { resource, actions: BTreeSet::from([action]) }),
            }
        }
    }
    Ok(scopes)
}

const EXPIRY_CHECK: &str = "check if time_ms($t), $t < {expires};";

fn expiry_param(expires_ms: i64) -> HashMap<String, Term> { HashMap::from([("expires".to_owned(), int(expires_ms))]) }

fn context_json(grant: &GrantId, holder: &PublicKey, expires_ms: i64, reshare: bool) -> Result<String> {
    Ok(serde_json::to_string(&BlockContext { g: grant.clone(), h: holder.to_string(), e: expires_ms, r: reshare })?)
}

/// One check that passes when the request falls in any of `scopes`. Values go
/// in as parameters, so names can't inject Datalog.
fn narrowing_check(scopes: &[Scope]) -> (String, HashMap<String, Term>) {
    let mut params = HashMap::new();
    let mut bodies = Vec::with_capacity(scopes.len());
    for (i, scope) in scopes.iter().enumerate() {
        let (kind, values) = scope.resource.parts();
        params.insert(format!("a{i}"), set(scope.actions.iter().map(|a| string(a.as_str())).collect()));
        for (j, value) in values.into_iter().enumerate() {
            params.insert(format!("v{i}x{j}"), value);
        }
        let body = kind.covers_with(|j| format!("{{v{i}x{j}}}"));
        bodies.push(format!("action($a), {body}, {{a{i}}}.contains($a)"));
    }
    let any = if bodies.is_empty() { "false".to_owned() } else { bodies.join(" or ") };
    (format!("check if {any};"), params)
}

/// The verifier's rules: which request facts each kind of right covers.
static RULES: LazyLock<String> = LazyLock::new(|| {
    let rules = KINDS.iter().map(|k| format!("granted($a) <- {}({}$a), {};\n", k.right, k.vars(), k.covers_with(|i| format!("$v{i}"))));
    rules.chain(["allow if action($a), granted($a);".to_owned()]).collect()
});

/// Grant ids this node knows to be revoked.
pub trait Revocations {
    fn is_revoked(&self, grant: &GrantId) -> bool;
}

impl<R: Revocations + ?Sized> Revocations for &R { fn is_revoked(&self, grant: &GrantId) -> bool { (**self).is_revoked(grant) } }

/// Why a token or a request was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("the token doesn't parse or isn't signed by this node: {0}")] BadToken(String),
    #[error("block {block} isn't signed by the holder before it")] BrokenChain { block: usize },
    #[error("block {block} re-shares a grant that may not be re-shared")] ReshareForbidden { block: usize },
    #[error("the token was issued to {holder}, not to {requester}")] NotHolder { holder: PublicKey, requester: PublicKey },
    #[error("the holder's proof doesn't match the challenge")] BadProof,
    #[error("grant {0} is revoked")] Revoked(GrantId),
    #[error("the token expired at {expired_ms}")] Expired { expired_ms: i64 },
    #[error("the grant has no `{action}` right on this {resource}")] NotGranted { action: Action, resource: &'static str },
    #[error("re-share {block} narrowed `{action}` on this {resource} away")] Narrowed { action: Action, resource: &'static str, block: usize },
    #[error("authorization failed: {0}")] Engine(String),
}

/// A token presented by a peer, with its proof over the verifier's challenge.
#[derive(Clone, Copy, Debug)]
pub struct Presented<'a> {
    pub token: &'a [u8],
    pub proof: &'a Proof,
}

/// Checks tokens presented to the node that issued them (the root).
pub struct Verifier<R> {
    root: PublicKey,
    revocations: R,
}

impl<R: Revocations> Verifier<R> {
    pub fn new(root: PublicKey, revocations: R) -> Self { Self { root, revocations } }
    /// Checks signatures, the holder chain, that `requester` is the last
    /// holder and signed `challenge`, revocation and expiry. Once per
    /// connection; then [`VerifiedGrant::authorize`] per request.
    pub fn verify(&self, presented: Presented<'_>, requester: &PublicKey, challenge: &Challenge, now_ms: i64) -> Result<VerifiedGrant, Denied> {
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
}

/// A token whose holder proved possession. Answers requests until it expires.
#[derive(Clone, Debug)]
pub struct VerifiedGrant {
    chain: Chain,
}

impl VerifiedGrant {
    /// The holders from the original grant to the presenter.
    pub fn chain(&self) -> &[Link] { &self.chain.links }
    /// When the token stops working: the earliest expiry in the chain.
    pub fn expires_ms(&self) -> i64 { self.chain.expires_ms() }
    fn live(&self, now_ms: i64) -> Result<(), Denied> {
        let expired_ms = self.expires_ms();
        if now_ms >= expired_ms { Err(Denied::Expired { expired_ms }) } else { Ok(()) }
    }

    /// Allows or denies `request` at `now_ms`.
    pub fn authorize(&self, request: &Request<'_>, now_ms: i64) -> Result<(), Denied> {
        self.live(now_ms)?;
        let action = request.action;
        let resource = match request.target {
            Target::Session { turn: Some(_), .. } => "prompt",
            Target::Session { .. } => "session",
            Target::Ssh { .. } => "ssh host",
            Target::Surface { .. } => "surface",
            Target::Sandbox => "sandbox",
            Target::Folder { .. } => "folder",
        };
        let not_granted = Denied::NotGranted { action, resource };
        let mut facts = vec![fact("action", &[string(action.as_str())]), fact("time_ms", &[int(now_ms)])];
        match request.target {
            Target::Session { session, turn } => {
                facts.extend([fact("session", &[string(&session.id)]), fact("created", &[int(session.created_ms)])]);
                facts.extend(session.project.iter().map(|project| fact("project", &[string(project)])));
                if let Some(turn) = turn {
                    // Ids above i64::MAX are in no grant (see `Scope::new`).
                    facts.push(fact("turn", &[int(i64::try_from(turn.0).map_err(|_| not_granted.clone())?)]));
                }
            }
            Target::Ssh { host } => facts.push(fact("ssh_host", &[string(host)])),
            Target::Surface { uri } => facts.push(fact("surface", &[string(uri)])),
            Target::Sandbox => facts.push(fact("sandbox", &[string(SANDBOX)])),
            // The folder and every folder above it: a grant on any covers it.
            Target::Folder { path } => {
                let ancestors = folder_ancestors(path).ok_or(not_granted.clone())?;
                facts.extend(ancestors.iter().map(|folder| fact("folder", &[string(folder)])));
            }
        }
        let engine = |e: BiscuitError| Denied::Engine(e.to_string());
        let limits = AuthorizerLimits { max_facts: 1000, max_iterations: 100, max_time: Duration::from_millis(250) };
        let mut builder = AuthorizerBuilder::new().code(RULES.as_str()).map_err(engine)?.set_limits(limits);
        for f in facts {
            builder = builder.fact(f).map_err(engine)?;
        }
        match builder.build(&self.chain.biscuit).map_err(engine)?.authorize() {
            Ok(_) => Ok(()),
            // The allow policy didn't match: the original grant lacks the right.
            Err(BiscuitError::FailedLogic(Logic::NoMatchingPolicy { .. })) => Err(not_granted),
            // Expiry was checked above, so a failed re-share block check is
            // its narrowing; a failed authority check is expiry.
            Err(BiscuitError::FailedLogic(Logic::Unauthorized { checks, .. })) => Err(checks
                .iter()
                .find_map(|c| match c {
                    FailedCheck::Block(b) if b.block_id > 0 => Some(Denied::Narrowed { action, resource, block: b.block_id as usize }),
                    _ => None,
                })
                .unwrap_or(Denied::Expired { expired_ms: self.expires_ms() })),
            Err(e) => Err(engine(e)),
        }
    }
}
