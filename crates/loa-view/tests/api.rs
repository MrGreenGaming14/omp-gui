//! The public API snapshot: renders every pub item under `src/` with `syn` and compares the
//! listing with `api.txt`, whose first line is `version = <loa_view::VERSION>`.
//!
//! - The listing changed and `VERSION` still equals the header: fails, asking for a bump.
//! - `VERSION` was bumped: `UPDATE_VIEW_API=1 cargo test -p loa-view --test api` rewrites
//!   `api.txt`; without it the test fails as stale. A removed or changed line needs a major
//!   bump, additions alone a minor one.

use quote::ToTokens;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use syn::{Attribute, Fields, ImplItem, Item, TraitItem, Visibility};

const HEADER: &str = "version = ";

#[test]
fn api_listing_matches_api_txt() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rendered = render(&root.join("src"));
    let path = root.join("api.txt");
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    let (saved_version, saved_body) = match saved.split_once('\n') {
        Some((head, body)) => (head.strip_prefix(HEADER).unwrap_or("").to_owned(), body.to_owned()),
        None => (String::new(), String::new()),
    };
    let version = loa_view::VERSION;
    if rendered == saved_body && version == saved_version {
        return;
    }
    let update = std::env::var_os("UPDATE_VIEW_API").is_some_and(|v| v == "1");
    let diff = diff(&saved_body, &rendered);
    if rendered != saved_body && version == saved_version {
        panic!("the loa-view API changed: bump loa_view::VERSION (major for removed or changed items, minor for additions), then run `UPDATE_VIEW_API=1 cargo test -p loa-view --test api`\n{diff}");
    }
    if !saved_version.is_empty() {
        let (old, new) = (semver(&saved_version), semver(version));
        assert!(new > old, "loa_view::VERSION {version} must be greater than api.txt's {saved_version}");
        let removed = diff.lines().any(|l| l.starts_with('-'));
        let added = diff.lines().any(|l| l.starts_with('+'));
        let major = |v: (u64, u64, u64)| if v.0 == 0 { (0, v.1) } else { (v.0, 0) };
        let minor = |v: (u64, u64, u64)| if v.0 == 0 { v } else { (v.0, v.1, 0) };
        assert!(
            !removed || major(new) > major(old),
            "items were removed or changed, which needs a major bump of loa_view::VERSION (from {saved_version}, now {version})\n{diff}"
        );
        assert!(
            !added || minor(new) > minor(old),
            "items were added, which needs at least a minor bump of loa_view::VERSION (from {saved_version}, now {version})\n{diff}"
        );
    }
    assert!(update, "api.txt is stale for loa_view::VERSION {version}: run `UPDATE_VIEW_API=1 cargo test -p loa-view --test api`\n{diff}");
    std::fs::write(&path, format!("{HEADER}{version}\n{rendered}")).unwrap();
}

fn semver(v: &str) -> (u64, u64, u64) {
    let parts: Vec<u64> = v.split('.').map(|p| p.parse().unwrap_or_else(|_| panic!("{v} is not semver"))).collect();
    let [major, minor, patch] = parts[..] else { panic!("{v} is not major.minor.patch") };
    (major, minor, patch)
}

/// Lines only in `old` as `-`, only in `new` as `+`, each prefixed with the item (and variant)
/// it belongs to.
fn diff(old: &str, new: &str) -> String {
    let (old, new) = (qualified(old), qualified(new));
    let mut out = String::new();
    old.difference(&new).for_each(|l| writeln!(out, "-{l}").unwrap());
    new.difference(&old).for_each(|l| writeln!(out, "+{l}").unwrap());
    out
}

fn qualified(listing: &str) -> BTreeSet<String> {
    let (mut item, mut member) = ("", "");
    listing
        .lines()
        .map(|line| match line.len() - line.trim_start().len() {
            0 => {
                item = line;
                line.to_owned()
            }
            2 => {
                member = line.trim();
                format!("{item} › {member}")
            }
            _ => format!("{item} › {member} › {}", line.trim()),
        })
        .collect()
}

/// What the listing collects: the public type names (impls of other types are skipped) and
/// the rendered blocks.
#[derive(Default)]
struct Listing {
    types: BTreeSet<String>,
    blocks: Vec<String>,
}

/// The listing of every module file under `src`, as blocks sorted by their first line.
fn render(src: &Path) -> String {
    let mut files = Vec::new();
    collect(src, &mut files);
    let mut parsed = Vec::new();
    for file in files {
        let rel = file.strip_prefix(src).unwrap().with_extension("");
        let mut module = vec!["loa_view".to_owned()];
        for part in rel.iter().map(|p| p.to_str().unwrap()) {
            if part != "lib" && part != "mod" {
                module.push(part.to_owned());
            }
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let file = syn::parse_file(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        parsed.push((module.join("::"), file));
    }
    let mut listing = Listing::default();
    parsed.iter().for_each(|(_, file)| public_types(&file.items, &mut listing.types));
    parsed.iter().for_each(|(module, file)| items(module, &file.items, &mut listing));
    listing.blocks.sort();
    listing.blocks.concat()
}

fn public_types(items: &[Item], out: &mut BTreeSet<String>) {
    for item in items {
        let (vis, ident) = match item {
            Item::Struct(i) => (&i.vis, &i.ident),
            Item::Enum(i) => (&i.vis, &i.ident),
            Item::Trait(i) => (&i.vis, &i.ident),
            Item::Type(i) => (&i.vis, &i.ident),
            Item::Union(i) => (&i.vis, &i.ident),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    public_types(inner, out);
                }
                continue;
            }
            _ => continue,
        };
        if public(vis) {
            out.insert(ident.to_string());
        }
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn items(module: &str, items: &[Item], listing: &mut Listing) {
    for item in items {
        if let Some(block) = render_item(module, item, listing) {
            listing.blocks.push(block);
        }
    }
}

fn render_item(module: &str, item: &Item, listing: &mut Listing) -> Option<String> {
    let mut s = String::new();
    match item {
        Item::Mod(m) if !is_test(&m.attrs) => {
            let path = format!("{module}::{}", m.ident);
            if let Some((_, inner)) = &m.content {
                items(&path, inner, listing);
            }
            if !public(&m.vis) {
                return None;
            }
            writeln!(s, "{}mod {path}", attrs(&m.attrs)).unwrap();
        }
        Item::Const(c) if public(&c.vis) => writeln!(s, "{}const {module}::{}: {}", attrs(&c.attrs), c.ident, tokens(&c.ty)).unwrap(),
        Item::Static(c) if public(&c.vis) => writeln!(s, "{}static {module}::{}: {}", attrs(&c.attrs), c.ident, tokens(&c.ty)).unwrap(),
        Item::Type(t) if public(&t.vis) => {
            writeln!(s, "{}type {module}::{}{} = {}", attrs(&t.attrs), t.ident, tokens(&t.generics), tokens(&t.ty)).unwrap()
        }
        Item::Fn(f) if public(&f.vis) => writeln!(s, "{}{}", attrs(&f.attrs), signature(module, &f.sig)).unwrap(),
        Item::Struct(st) if public(&st.vis) => {
            let generics = generics(&st.generics);
            writeln!(s, "{}struct {module}::{}{generics}", attrs(&st.attrs), st.ident).unwrap();
            fields(&st.fields, "  ", &mut s, false);
        }
        Item::Enum(e) if public(&e.vis) => {
            writeln!(s, "{}enum {module}::{}{}", attrs(&e.attrs), e.ident, generics(&e.generics)).unwrap();
            for v in &e.variants {
                writeln!(s, "  {}{}", attrs(&v.attrs), v.ident).unwrap();
                fields(&v.fields, "    ", &mut s, true);
            }
        }
        Item::Trait(t) if public(&t.vis) => {
            let bounds = if t.supertraits.is_empty() { String::new() } else { format!(": {}", tokens(&t.supertraits)) };
            writeln!(s, "{}trait {module}::{}{}{bounds}", attrs(&t.attrs), t.ident, generics(&t.generics)).unwrap();
            for item in &t.items {
                match item {
                    TraitItem::Fn(f) => writeln!(s, "  {}{}", attrs(&f.attrs), signature("", &f.sig)).unwrap(),
                    TraitItem::Type(ty) => writeln!(s, "  type {}{}", ty.ident, if ty.bounds.is_empty() { String::new() } else { format!(": {}", tokens(&ty.bounds)) }).unwrap(),
                    TraitItem::Const(c) => writeln!(s, "  const {}: {}", c.ident, tokens(&c.ty)).unwrap(),
                    other => writeln!(s, "  {}", tokens(other)).unwrap(),
                }
            }
        }
        Item::Impl(i) if !is_test(&i.attrs) && self_name(&i.self_ty).is_some_and(|n| listing.types.contains(&n)) => {
            let trait_ = i.trait_.as_ref().map(|(bang, path, _)| format!("{}{} for ", if bang.is_some() { "!" } else { "" }, tokens(path)));
            let mut body = String::new();
            for item in &i.items {
                match item {
                    ImplItem::Fn(f) if trait_.is_some() || public(&f.vis) => writeln!(body, "  {}{}", attrs(&f.attrs), signature("", &f.sig)).unwrap(),
                    ImplItem::Const(c) if trait_.is_some() || public(&c.vis) => writeln!(body, "  const {}: {}", c.ident, tokens(&c.ty)).unwrap(),
                    ImplItem::Type(t) if trait_.is_some() || public(&t.vis) => writeln!(body, "  type {} = {}", t.ident, tokens(&t.ty)).unwrap(),
                    _ => {}
                }
            }
            if trait_.is_none() && body.is_empty() {
                return None;
            }
            let trait_ = trait_.unwrap_or_default();
            writeln!(s, "{}impl{} {trait_}{module}::{}", attrs(&i.attrs), generics(&i.generics), tokens(&i.self_ty)).unwrap();
            s.push_str(&body);
        }
        Item::Use(u) if public(&u.vis) => {
            let mut leaves = Vec::new();
            use_tree(String::new(), &u.tree, &mut leaves);
            for (name, source) in leaves {
                writeln!(s, "{}use {module}::{name} = {source}", attrs(&u.attrs)).unwrap();
            }
        }
        Item::Macro(m) if m.attrs.iter().any(|a| a.path().is_ident("macro_export")) => {
            writeln!(s, "macro loa_view::{}!", m.ident.as_ref().map(ToString::to_string).unwrap_or_default()).unwrap()
        }
        _ => return None,
    }
    Some(s)
}

/// The last path segment of an impl's self type.
fn self_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn use_tree(prefix: String, tree: &syn::UseTree, out: &mut Vec<(String, String)>) {
    let join = |p: &str, s: &str| if p.is_empty() { s.to_owned() } else { format!("{p}::{s}") };
    match tree {
        syn::UseTree::Path(p) => use_tree(join(&prefix, &p.ident.to_string()), &p.tree, out),
        syn::UseTree::Name(n) => out.push((n.ident.to_string(), join(&prefix, &n.ident.to_string()))),
        syn::UseTree::Rename(r) => out.push((r.rename.to_string(), join(&prefix, &r.ident.to_string()))),
        syn::UseTree::Glob(_) => out.push(("*".to_owned(), join(&prefix, "*"))),
        syn::UseTree::Group(g) => g.items.iter().for_each(|t| use_tree(prefix.clone(), t, out)),
    }
}

/// Variant fields have no visibility of their own and are always public.
fn fields(fields: &Fields, indent: &str, s: &mut String, variant: bool) {
    let mut hidden = false;
    for (i, f) in fields.iter().enumerate() {
        if !variant && !public(&f.vis) {
            hidden = true;
            continue;
        }
        let name = f.ident.as_ref().map_or(i.to_string(), ToString::to_string);
        writeln!(s, "{indent}{}{name}: {}", attrs(&f.attrs), tokens(&f.ty)).unwrap();
    }
    if hidden {
        writeln!(s, "{indent}..").unwrap();
    }
}

fn signature(module: &str, sig: &syn::Signature) -> String {
    let mut sig = sig.clone();
    for input in sig.inputs.iter_mut() {
        if let syn::FnArg::Typed(t) = input {
            *t.pat = syn::parse_quote!(_);
        }
    }
    let rendered = tokens(&sig);
    if module.is_empty() { rendered } else { rendered.replacen(&format!("fn {}", sig.ident), &format!("fn {module}::{}", sig.ident), 1) }
}

fn generics(g: &syn::Generics) -> String {
    let params = tokens(g);
    match &g.where_clause {
        Some(w) => format!("{params} {}", tokens(w)),
        None => params,
    }
}

/// The attributes that change the API: derives, cfgs and `non_exhaustive`, docs dropped.
fn attrs(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .filter(|a| ["derive", "cfg", "non_exhaustive", "repr"].iter().any(|n| a.path().is_ident(n)))
        .map(|a| format!("{} ", tokens(a)))
        .collect()
}

/// `#[cfg(test)]` and anything gated on `test`.
fn is_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("cfg") && tokens(&a.meta).split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "test"))
}

fn public(vis: &Visibility) -> bool { matches!(vis, Visibility::Public(_)) }

/// Tokens printed with the spacing `rustfmt` would use around paths and brackets.
fn tokens(t: &impl ToTokens) -> String {
    let mut s = t.to_token_stream().to_string();
    for (from, to) in [(" :: ", "::"), (":: ", "::"), (" < ", "<"), ("< ", "<"), (" >", ">"), (" ,", ","), ("& ", "&"), (" (", "("), ("( ", "("), (" )", ")"), ("[ ", "["), (" ]", "]"), ("# ", "#"), ("! ", "!"), (" ;", ";")] {
        s = s.replace(from, to);
    }
    s.replace(" : ", ": ").replace("->", " -> ").replace("  ", " ")
}
