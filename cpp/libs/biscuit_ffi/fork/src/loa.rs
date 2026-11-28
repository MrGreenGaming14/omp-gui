/*
 * loa additions to biscuit-capi: the biscuit-auth 6.0 surface that
 * crates/loa-trust/src/token.rs uses (third-party blocks, block context,
 * external keys, parameterized code, authorizer limits, queries).
 * Declared in cpp/libs/biscuit_ffi/include/loa/biscuit_ffi/loa_biscuit.h.
 *
 * Conventions: functions returning `LoaBscStatus` report details through the
 * thread's last error (`loa_bsc_error_message`, `loa_bsc_error_failed_check*`).
 * Functions returning a pointer return NULL on error with the same reporting.
 * Every handle is freed with its `_free` function; NULL is accepted there.
 */
use biscuit_auth::builder::{self, Term};
use biscuit_auth::error::{FailedCheck, Format, Logic, RunLimit, Token};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoaBscStatus {
    Ok = 0,
    InvalidArgument = 1,
    Internal = 2,
    Format = 3,
    FormatSignature = 4,
    FormatUnknownPublicKey = 5,
    AppendOnSealed = 6,
    AlreadySealed = 7,
    NoMatchingPolicy = 8,
    Unauthorized = 9,
    InvalidBlockRule = 10,
    AuthorizerNotEmpty = 11,
    Language = 12,
    TooManyFacts = 13,
    TooManyIterations = 14,
    Timeout = 15,
    UnexpectedQueryResult = 16,
    Conversion = 17,
    Base64 = 18,
    Execution = 19,
}

#[derive(Default)]
struct LastError {
    status: Option<LoaBscStatus>,
    message: Vec<u8>, // NUL-terminated
    checks: Vec<(bool, u32, u32, Vec<u8>)>, // (is_block, block_id, check_id, rule\0)
}

thread_local! {
    static LAST: RefCell<LastError> = RefCell::new(LastError::default());
}

fn cbytes(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.bytes().filter(|b| *b != 0).collect();
    v.push(0);
    v
}

fn status_of(e: &Token) -> LoaBscStatus {
    use LoaBscStatus as S;
    match e {
        Token::InternalError => S::Internal,
        Token::Format(Format::Signature(_)) | Token::Format(Format::SealedSignature) => S::FormatSignature,
        Token::Format(Format::UnknownPublicKey) | Token::Format(Format::UnknownExternalKey) => S::FormatUnknownPublicKey,
        Token::Format(_) => S::Format,
        Token::AppendOnSealed => S::AppendOnSealed,
        Token::AlreadySealed => S::AlreadySealed,
        Token::FailedLogic(Logic::NoMatchingPolicy { .. }) => S::NoMatchingPolicy,
        Token::FailedLogic(Logic::Unauthorized { .. }) => S::Unauthorized,
        Token::FailedLogic(Logic::InvalidBlockRule(..)) => S::InvalidBlockRule,
        Token::FailedLogic(Logic::AuthorizerNotEmpty) => S::AuthorizerNotEmpty,
        Token::Language(_) => S::Language,
        Token::RunLimit(RunLimit::TooManyFacts) => S::TooManyFacts,
        Token::RunLimit(RunLimit::TooManyIterations) => S::TooManyIterations,
        Token::RunLimit(RunLimit::Timeout) => S::Timeout,
        Token::RunLimit(RunLimit::UnexpectedQueryResult(..)) => S::UnexpectedQueryResult,
        Token::ConversionError(_) => S::Conversion,
        Token::Base64(_) => S::Base64,
        Token::Execution(_) => S::Execution,
    }
}

fn set_error(status: LoaBscStatus, message: &str, checks: &[FailedCheck]) -> LoaBscStatus {
    LAST.with(|l| {
        let mut l = l.borrow_mut();
        l.status = Some(status);
        l.message = cbytes(message);
        l.checks = checks
            .iter()
            .map(|c| match c {
                FailedCheck::Block(b) => (true, b.block_id, b.check_id, cbytes(&b.rule)),
                FailedCheck::Authorizer(a) => (false, 0, a.check_id, cbytes(&a.rule)),
            })
            .collect();
    });
    status
}

fn fail(e: Token) -> LoaBscStatus {
    let checks: &[FailedCheck] = match &e {
        Token::FailedLogic(Logic::NoMatchingPolicy { checks }) | Token::FailedLogic(Logic::Unauthorized { checks, .. }) => checks,
        _ => &[],
    };
    set_error(status_of(&e), &e.to_string(), checks)
}

fn invalid(what: &str) -> LoaBscStatus { set_error(LoaBscStatus::InvalidArgument, &format!("invalid argument: {what}"), &[]) }

fn clear() {
    LAST.with(|l| *l.borrow_mut() = LastError::default());
}

/// Runs `f`, recording its error; NULL on error.
fn boxed<T>(r: Result<T, LoaBscStatus>) -> *mut T {
    match r {
        Ok(v) => {
            clear();
            Box::into_raw(Box::new(v))
        }
        Err(_) => std::ptr::null_mut(),
    }
}

fn status(r: Result<(), LoaBscStatus>) -> LoaBscStatus {
    match r {
        Ok(()) => {
            clear();
            LoaBscStatus::Ok
        }
        Err(s) => s,
    }
}

unsafe fn str_arg<'a>(p: *const c_char, what: &str) -> Result<&'a str, LoaBscStatus> {
    if p.is_null() {
        return Err(invalid(what));
    }
    CStr::from_ptr(p).to_str().map_err(|_| invalid(what))
}

unsafe fn bytes_arg<'a>(p: *const u8, len: usize) -> &'a [u8] { if len == 0 { &[] } else { std::slice::from_raw_parts(p, len) } }

unsafe fn ref_arg<'a, T>(p: *const T, what: &str) -> Result<&'a T, LoaBscStatus> { p.as_ref().ok_or_else(|| invalid(what)) }

unsafe fn mut_arg<'a, T>(p: *mut T, what: &str) -> Result<&'a mut T, LoaBscStatus> { p.as_mut().ok_or_else(|| invalid(what)) }

// ---- errors ---------------------------------------------------------------

/// Status of the thread's last failed call, `Ok` if the last call succeeded.
#[no_mangle]
pub extern "C" fn loa_bsc_error_status() -> LoaBscStatus { LAST.with(|l| l.borrow().status.unwrap_or(LoaBscStatus::Ok)) }

/// Message of the thread's last error ("" if none). Valid until the next call.
#[no_mangle]
pub extern "C" fn loa_bsc_error_message() -> *const c_char {
    LAST.with(|l| {
        let l = l.borrow();
        if l.message.is_empty() { b"\0".as_ptr() as *const c_char } else { l.message.as_ptr() as *const c_char }
    })
}

/// Failed checks carried by the last NoMatchingPolicy/Unauthorized error.
#[no_mangle]
pub extern "C" fn loa_bsc_error_failed_check_count() -> usize { LAST.with(|l| l.borrow().checks.len()) }

/// One failed check. `is_block` false means an authorizer check (block_id 0).
/// `rule` is valid until the next call. Returns false if `index` is out of range.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_error_failed_check(index: usize, is_block: *mut bool, block_id: *mut u32, check_id: *mut u32, rule: *mut *const c_char) -> bool {
    LAST.with(|l| {
        let l = l.borrow();
        let Some((b, block, check, text)) = l.checks.get(index) else { return false };
        if !is_block.is_null() {
            *is_block = *b;
        }
        if !block_id.is_null() {
            *block_id = *block;
        }
        if !check_id.is_null() {
            *check_id = *check;
        }
        if !rule.is_null() {
            *rule = text.as_ptr() as *const c_char;
        }
        true
    })
}

// ---- byte buffers ---------------------------------------------------------

#[repr(C)]
pub struct LoaBscBytes {
    pub data: *mut u8,
    pub len: usize,
}

fn to_bytes(v: Vec<u8>) -> LoaBscBytes {
    let mut b = v.into_boxed_slice();
    let out = LoaBscBytes { data: b.as_mut_ptr(), len: b.len() };
    std::mem::forget(b);
    out
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_bytes_free(bytes: LoaBscBytes) {
    if !bytes.data.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(bytes.data, bytes.len)));
    }
}

const NO_BYTES: LoaBscBytes = LoaBscBytes { data: std::ptr::null_mut(), len: 0 };

// ---- keys -----------------------------------------------------------------

pub struct LoaBscKeyPair(biscuit_auth::KeyPair);
pub struct LoaBscPublicKey(biscuit_auth::PublicKey);

/// Ed25519 key pair from a 32-byte secret seed.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_key_pair_from_seed(seed: *const u8, len: usize) -> *mut LoaBscKeyPair {
    boxed((|| {
        if seed.is_null() || len != 32 {
            return Err(invalid("seed must be 32 bytes"));
        }
        let private = biscuit_auth::PrivateKey::from_bytes(bytes_arg(seed, len), biscuit_auth::Algorithm::Ed25519).map_err(|e| fail(e.into()))?;
        Ok(LoaBscKeyPair(biscuit_auth::KeyPair::from(&private)))
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_key_pair_public(kp: *const LoaBscKeyPair) -> *mut LoaBscPublicKey {
    boxed((|| Ok(LoaBscPublicKey(ref_arg(kp, "key pair")?.0.public())))())
}

/// Signs `data` with the private key: 64-byte Ed25519 signature.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_key_pair_sign(kp: *const LoaBscKeyPair, data: *const u8, len: usize, out: *mut LoaBscBytes) -> LoaBscStatus {
    status((|| {
        let kp = ref_arg(kp, "key pair")?;
        let out = mut_arg(out, "out")?;
        let sig = kp.0.sign(bytes_arg(data, len)).map_err(|e| fail(e.into()))?;
        *out = to_bytes(sig.to_bytes().to_vec());
        Ok(())
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_key_pair_free(kp: *mut LoaBscKeyPair) {
    if !kp.is_null() {
        drop(Box::from_raw(kp));
    }
}

/// Ed25519 public key from 32 bytes.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_public_key_from_bytes(data: *const u8, len: usize) -> *mut LoaBscPublicKey {
    boxed((|| {
        if data.is_null() || len != 32 {
            return Err(invalid("public key must be 32 bytes"));
        }
        let key = biscuit_auth::PublicKey::from_bytes(bytes_arg(data, len), biscuit_auth::Algorithm::Ed25519).map_err(|e| fail(e.into()))?;
        Ok(LoaBscPublicKey(key))
    })())
}

/// Raw key bytes (32 for Ed25519) into `out[0..32]`. Returns the length written, 0 on error.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_public_key_bytes(key: *const LoaBscPublicKey, out: *mut u8) -> usize {
    let Some(key) = key.as_ref() else {
        invalid("public key");
        return 0;
    };
    let bytes = key.0.to_bytes();
    if bytes.len() != 32 || out.is_null() {
        invalid("not an ed25519 key");
        return 0;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 32);
    clear();
    32
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_public_key_free(key: *mut LoaBscPublicKey) {
    if !key.is_null() {
        drop(Box::from_raw(key));
    }
}

// ---- terms and parameters ---------------------------------------------------

pub struct LoaBscTerm(Term);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoaBscTermKind {
    Other = 0,
    String = 1,
    Integer = 2,
    Set = 3,
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_string(s: *const c_char) -> *mut LoaBscTerm { boxed((|| Ok(LoaBscTerm(builder::string(str_arg(s, "string")?))))()) }

#[no_mangle]
pub extern "C" fn loa_bsc_term_int(v: i64) -> *mut LoaBscTerm { boxed(Ok(LoaBscTerm(builder::int(v)))) }

/// An empty set; add elements with `loa_bsc_term_set_insert`.
#[no_mangle]
pub extern "C" fn loa_bsc_term_set() -> *mut LoaBscTerm { boxed(Ok(LoaBscTerm(Term::Set(BTreeSet::new())))) }

/// Copies `element` into `set`.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_set_insert(set: *mut LoaBscTerm, element: *const LoaBscTerm) -> LoaBscStatus {
    status((|| {
        let element = ref_arg(element, "element")?.0.clone();
        match &mut mut_arg(set, "set")?.0 {
            Term::Set(s) => {
                s.insert(element);
                Ok(())
            }
            _ => Err(invalid("not a set")),
        }
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_clone(term: *const LoaBscTerm) -> *mut LoaBscTerm { boxed((|| Ok(LoaBscTerm(ref_arg(term, "term")?.0.clone())))()) }

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_kind(term: *const LoaBscTerm) -> LoaBscTermKind {
    match term.as_ref().map(|t| &t.0) {
        Some(Term::Str(_)) => LoaBscTermKind::String,
        Some(Term::Integer(_)) => LoaBscTermKind::Integer,
        Some(Term::Set(_)) => LoaBscTermKind::Set,
        _ => LoaBscTermKind::Other,
    }
}

/// String bytes (not NUL-terminated), borrowed from `term`. False if not a string.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_as_string(term: *const LoaBscTerm, data: *mut *const c_char, len: *mut usize) -> bool {
    match term.as_ref().map(|t| &t.0) {
        Some(Term::Str(s)) if !data.is_null() && !len.is_null() => {
            *data = s.as_ptr() as *const c_char;
            *len = s.len();
            true
        }
        _ => false,
    }
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_as_int(term: *const LoaBscTerm, out: *mut i64) -> bool {
    match term.as_ref().map(|t| &t.0) {
        Some(Term::Integer(v)) if !out.is_null() => {
            *out = *v;
            true
        }
        _ => false,
    }
}

/// Number of elements of a set term, 0 for anything else.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_set_len(term: *const LoaBscTerm) -> usize {
    match term.as_ref().map(|t| &t.0) {
        Some(Term::Set(s)) => s.len(),
        _ => 0,
    }
}

/// Element `index` of a set term in its sorted order, as a new term.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_set_at(term: *const LoaBscTerm, index: usize) -> *mut LoaBscTerm {
    boxed((|| match &ref_arg(term, "term")?.0 {
        Term::Set(s) => s.iter().nth(index).cloned().map(LoaBscTerm).ok_or_else(|| invalid("index")),
        _ => Err(invalid("not a set")),
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_term_free(term: *mut LoaBscTerm) {
    if !term.is_null() {
        drop(Box::from_raw(term));
    }
}

pub struct LoaBscParams(HashMap<String, Term>);

#[no_mangle]
pub extern "C" fn loa_bsc_params_new() -> *mut LoaBscParams { boxed(Ok(LoaBscParams(HashMap::new()))) }

/// Binds `{name}` to a copy of `term`.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_params_insert(params: *mut LoaBscParams, name: *const c_char, term: *const LoaBscTerm) -> LoaBscStatus {
    status((|| {
        let name = str_arg(name, "name")?.to_owned();
        let term = ref_arg(term, "term")?.0.clone();
        mut_arg(params, "params")?.0.insert(name, term);
        Ok(())
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_params_free(params: *mut LoaBscParams) {
    if !params.is_null() {
        drop(Box::from_raw(params));
    }
}

unsafe fn fact_arg(name: *const c_char, terms: *const *const LoaBscTerm, count: usize) -> Result<builder::Fact, LoaBscStatus> {
    let name = str_arg(name, "fact name")?;
    let mut list = Vec::with_capacity(count);
    for i in 0..count {
        list.push(ref_arg(*terms.add(i), "fact term")?.0.clone());
    }
    Ok(builder::fact(name, &list))
}

unsafe fn params_arg(params: *const LoaBscParams) -> HashMap<String, Term> { params.as_ref().map(|p| p.0.clone()).unwrap_or_default() }

/// Applies a by-value builder step to `slot`, keeping the builder on error.
fn step<B: Clone>(slot: &mut B, f: impl FnOnce(B) -> Result<B, Token>) -> Result<(), LoaBscStatus> {
    *slot = f(slot.clone()).map_err(fail)?;
    Ok(())
}

// ---- biscuit builder ----------------------------------------------------------

pub struct LoaBscBiscuitBuilder(builder::BiscuitBuilder);

#[no_mangle]
pub extern "C" fn loa_bsc_biscuit_builder_new() -> *mut LoaBscBiscuitBuilder { boxed(Ok(LoaBscBiscuitBuilder(builder::BiscuitBuilder::new()))) }

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_builder_fact(b: *mut LoaBscBiscuitBuilder, name: *const c_char, terms: *const *const LoaBscTerm, count: usize) -> LoaBscStatus {
    status((|| {
        let fact = fact_arg(name, terms, count)?;
        step(&mut mut_arg(b, "builder")?.0, |b| b.fact(fact))
    })())
}

/// Datalog source with `{name}` parameters bound from `params` (may be NULL).
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_builder_code(b: *mut LoaBscBiscuitBuilder, source: *const c_char, params: *const LoaBscParams) -> LoaBscStatus {
    status((|| {
        let source = str_arg(source, "source")?;
        let params = params_arg(params);
        step(&mut mut_arg(b, "builder")?.0, |b| b.code_with_params(source, params, HashMap::new()))
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_builder_context(b: *mut LoaBscBiscuitBuilder, context: *const c_char) -> LoaBscStatus {
    status((|| {
        let context = str_arg(context, "context")?.to_owned();
        let b = mut_arg(b, "builder")?;
        b.0 = b.0.clone().context(context);
        Ok(())
    })())
}

/// Signs the authority block with `root`.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_builder_build(b: *const LoaBscBiscuitBuilder, root: *const LoaBscKeyPair) -> *mut LoaBscBiscuit {
    boxed((|| {
        let b = ref_arg(b, "builder")?;
        let root = ref_arg(root, "root key pair")?;
        b.0.clone().build(&root.0).map(LoaBscBiscuit).map_err(fail)
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_builder_free(b: *mut LoaBscBiscuitBuilder) {
    if !b.is_null() {
        drop(Box::from_raw(b));
    }
}

// ---- block builder ------------------------------------------------------------

pub struct LoaBscBlockBuilder(builder::BlockBuilder);

#[no_mangle]
pub extern "C" fn loa_bsc_block_builder_new() -> *mut LoaBscBlockBuilder { boxed(Ok(LoaBscBlockBuilder(builder::BlockBuilder::new()))) }

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_block_builder_fact(b: *mut LoaBscBlockBuilder, name: *const c_char, terms: *const *const LoaBscTerm, count: usize) -> LoaBscStatus {
    status((|| {
        let fact = fact_arg(name, terms, count)?;
        step(&mut mut_arg(b, "builder")?.0, |b| b.fact(fact))
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_block_builder_code(b: *mut LoaBscBlockBuilder, source: *const c_char, params: *const LoaBscParams) -> LoaBscStatus {
    status((|| {
        let source = str_arg(source, "source")?;
        let params = params_arg(params);
        step(&mut mut_arg(b, "builder")?.0, |b| b.code_with_params(source, params, HashMap::new()))
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_block_builder_context(b: *mut LoaBscBlockBuilder, context: *const c_char) -> LoaBscStatus {
    status((|| {
        let context = str_arg(context, "context")?.to_owned();
        let b = mut_arg(b, "builder")?;
        b.0 = b.0.clone().context(context);
        Ok(())
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_block_builder_free(b: *mut LoaBscBlockBuilder) {
    if !b.is_null() {
        drop(Box::from_raw(b));
    }
}

// ---- biscuit --------------------------------------------------------------------

pub struct LoaBscBiscuit(biscuit_auth::Biscuit);

/// Parses `data` and verifies every signature against `root`.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_from(data: *const u8, len: usize, root: *const LoaBscPublicKey) -> *mut LoaBscBiscuit {
    boxed((|| {
        let root = ref_arg(root, "root key")?;
        biscuit_auth::Biscuit::from(bytes_arg(data, len), root.0).map(LoaBscBiscuit).map_err(fail)
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_to_vec(b: *const LoaBscBiscuit, out: *mut LoaBscBytes) -> LoaBscStatus {
    status((|| {
        let b = ref_arg(b, "biscuit")?;
        let out = mut_arg(out, "out")?;
        *out = to_bytes(b.0.to_vec().map_err(fail)?);
        Ok(())
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_block_count(b: *const LoaBscBiscuit) -> usize { b.as_ref().map(|b| b.0.block_count()).unwrap_or(0) }

/// Context string of block `index`, copied into `out` (`data` NULL when the
/// block has none). Returns InvalidArgument for an out-of-range index.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_block_context(b: *const LoaBscBiscuit, index: usize, out: *mut LoaBscBytes) -> LoaBscStatus {
    status((|| {
        let b = ref_arg(b, "biscuit")?;
        let out = mut_arg(out, "out")?;
        let all = b.0.context();
        let ctx = all.get(index).ok_or_else(|| invalid("block index"))?;
        *out = ctx.as_ref().map(|c| to_bytes(c.as_bytes().to_vec())).unwrap_or(NO_BYTES);
        Ok(())
    })())
}

/// External (third-party) signer of block `index`, NULL with status Ok when
/// the block has none (check `loa_bsc_error_status`).
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_block_external_key(b: *const LoaBscBiscuit, index: usize) -> *mut LoaBscPublicKey {
    let r = (|| {
        let b = ref_arg(b, "biscuit")?;
        let keys = b.0.external_public_keys();
        keys.get(index).cloned().ok_or_else(|| invalid("block index"))
    })();
    match r {
        Ok(Some(k)) => boxed(Ok(LoaBscPublicKey(k))),
        Ok(None) => {
            clear();
            std::ptr::null_mut()
        }
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_free(b: *mut LoaBscBiscuit) {
    if !b.is_null() {
        drop(Box::from_raw(b));
    }
}

// ---- third party ----------------------------------------------------------------

pub struct LoaBscThirdPartyRequest(biscuit_auth::ThirdPartyRequest);
pub struct LoaBscThirdPartyBlock(biscuit_auth::ThirdPartyBlock);

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_third_party_request(b: *const LoaBscBiscuit) -> *mut LoaBscThirdPartyRequest {
    boxed((|| ref_arg(b, "biscuit")?.0.third_party_request().map(LoaBscThirdPartyRequest).map_err(fail))())
}

/// Signs `block` with `signer`'s private key. Consumes `request` (always; do
/// not free it afterwards).
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_third_party_request_create_block(request: *mut LoaBscThirdPartyRequest, signer: *const LoaBscKeyPair, block: *const LoaBscBlockBuilder) -> *mut LoaBscThirdPartyBlock {
    if request.is_null() {
        invalid("request");
        return std::ptr::null_mut();
    }
    let request = Box::from_raw(request);
    boxed((|| {
        let signer = ref_arg(signer, "signer")?;
        let block = ref_arg(block, "block")?;
        request.0.create_block(&signer.0.private(), block.0.clone()).map(LoaBscThirdPartyBlock).map_err(fail)
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_third_party_request_free(request: *mut LoaBscThirdPartyRequest) {
    if !request.is_null() {
        drop(Box::from_raw(request));
    }
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_third_party_block_free(block: *mut LoaBscThirdPartyBlock) {
    if !block.is_null() {
        drop(Box::from_raw(block));
    }
}

/// A new token: `b` plus `block`, signed by the holder of `signer`.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_biscuit_append_third_party(b: *const LoaBscBiscuit, signer: *const LoaBscPublicKey, block: *const LoaBscThirdPartyBlock) -> *mut LoaBscBiscuit {
    boxed((|| {
        let b = ref_arg(b, "biscuit")?;
        let signer = ref_arg(signer, "signer")?;
        let block = ref_arg(block, "block")?;
        b.0.append_third_party(signer.0, block.0.clone()).map(LoaBscBiscuit).map_err(fail)
    })())
}

// ---- authorizer -----------------------------------------------------------------

pub struct LoaBscAuthorizerBuilder(builder::AuthorizerBuilder);
pub struct LoaBscAuthorizer(biscuit_auth::Authorizer);

#[no_mangle]
pub extern "C" fn loa_bsc_authorizer_builder_new() -> *mut LoaBscAuthorizerBuilder { boxed(Ok(LoaBscAuthorizerBuilder(builder::AuthorizerBuilder::new()))) }

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_builder_code(b: *mut LoaBscAuthorizerBuilder, source: *const c_char, params: *const LoaBscParams) -> LoaBscStatus {
    status((|| {
        let source = str_arg(source, "source")?;
        let params = params_arg(params);
        step(&mut mut_arg(b, "builder")?.0, |b| b.code_with_params(source, params, HashMap::new()))
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_builder_fact(b: *mut LoaBscAuthorizerBuilder, name: *const c_char, terms: *const *const LoaBscTerm, count: usize) -> LoaBscStatus {
    status((|| {
        let fact = fact_arg(name, terms, count)?;
        step(&mut mut_arg(b, "builder")?.0, |b| b.fact(fact))
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_builder_set_limits(b: *mut LoaBscAuthorizerBuilder, max_facts: u64, max_iterations: u64, max_time_us: u64) -> LoaBscStatus {
    status((|| {
        let b = mut_arg(b, "builder")?;
        let limits = biscuit_auth::AuthorizerLimits { max_facts, max_iterations, max_time: Duration::from_micros(max_time_us) };
        b.0 = b.0.clone().set_limits(limits);
        Ok(())
    })())
}

/// Loads `token` (may be NULL for an unauthenticated authorizer).
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_builder_build(b: *const LoaBscAuthorizerBuilder, token: *const LoaBscBiscuit) -> *mut LoaBscAuthorizer {
    boxed((|| {
        let b = ref_arg(b, "builder")?.0.clone();
        match token.as_ref() {
            Some(t) => b.build(&t.0),
            None => b.build_unauthenticated(),
        }
        .map(LoaBscAuthorizer)
        .map_err(fail)
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_builder_free(b: *mut LoaBscAuthorizerBuilder) {
    if !b.is_null() {
        drop(Box::from_raw(b));
    }
}

/// Runs checks and policies. On success writes the matched allow policy's index.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_authorize(a: *mut LoaBscAuthorizer, policy: *mut usize) -> LoaBscStatus {
    status((|| {
        let index = mut_arg(a, "authorizer")?.0.authorize().map_err(fail)?;
        if !policy.is_null() {
            *policy = index;
        }
        Ok(())
    })())
}

pub struct LoaBscFacts(Vec<(String, Vec<LoaBscTerm>)>);

/// Facts produced by `rule`, e.g. `r($a) <- right($a)`.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_query(a: *mut LoaBscAuthorizer, rule: *const c_char) -> *mut LoaBscFacts {
    boxed((|| {
        let rule = str_arg(rule, "rule")?;
        let a = mut_arg(a, "authorizer")?;
        let facts: Vec<builder::Fact> = a.0.query(rule).map_err(fail)?;
        Ok(LoaBscFacts(facts.into_iter().map(|f| (f.predicate.name, f.predicate.terms.into_iter().map(LoaBscTerm).collect())).collect()))
    })())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_authorizer_free(a: *mut LoaBscAuthorizer) {
    if !a.is_null() {
        drop(Box::from_raw(a));
    }
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_facts_len(f: *const LoaBscFacts) -> usize { f.as_ref().map(|f| f.0.len()).unwrap_or(0) }

/// Predicate name of fact `index` (NUL-free bytes, borrowed). False if out of range.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_facts_name(f: *const LoaBscFacts, index: usize, data: *mut *const c_char, len: *mut usize) -> bool {
    match f.as_ref().and_then(|f| f.0.get(index)) {
        Some((name, _)) if !data.is_null() && !len.is_null() => {
            *data = name.as_ptr() as *const c_char;
            *len = name.len();
            true
        }
        _ => false,
    }
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_facts_term_count(f: *const LoaBscFacts, index: usize) -> usize { f.as_ref().and_then(|f| f.0.get(index)).map(|(_, t)| t.len()).unwrap_or(0) }

/// Term `term` of fact `index`, borrowed from `f`; NULL if out of range.
#[no_mangle]
pub unsafe extern "C" fn loa_bsc_facts_term(f: *const LoaBscFacts, index: usize, term: usize) -> *const LoaBscTerm {
    f.as_ref().and_then(|f| f.0.get(index)).and_then(|(_, t)| t.get(term)).map(|t| t as *const LoaBscTerm).unwrap_or(std::ptr::null())
}

#[no_mangle]
pub unsafe extern "C" fn loa_bsc_facts_free(f: *mut LoaBscFacts) {
    if !f.is_null() {
        drop(Box::from_raw(f));
    }
}
