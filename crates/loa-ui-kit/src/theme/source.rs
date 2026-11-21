//! Ports for where themes come from, and the registry that resolves a config [`ThemeRef`].

use super::{BuiltinTheme, ThemeError, TokenSet, Variant};
use loa_util::registry::Registry;
use loa_view::theme::{Appearance, ThemeRef};
use std::sync::Arc;

/// A theme: named token sets per variant. Builtins today; a script adapter later.
pub trait ThemeSource: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn variants(&self) -> &[Variant];
    fn tokens(&self, v: Variant) -> Result<TokenSet, ThemeError>;
}

/// Turns a ref the registry does not know (a file path, a script) into a source.
pub trait ThemeLoader: Send + Sync {
    fn can_load(&self, r: &ThemeRef) -> bool;
    fn load(&self, r: &ThemeRef) -> Result<Arc<dyn ThemeSource>, ThemeError>;
}

/// Named theme sources plus loaders for everything else.
#[derive(Default)]
pub struct ThemeRegistry {
    sources: Registry<dyn ThemeSource>,
    loaders: Vec<Box<dyn ThemeLoader>>,
}

impl ThemeRegistry {
    /// `system`, `dark` and `light`.
    pub fn builtin() -> Self {
        let mut r = Self::default();
        BuiltinTheme::ALL.into_iter().for_each(|t| r.register(Arc::new(t)));
        r
    }

    pub fn register(&mut self, s: Arc<dyn ThemeSource>) { self.sources.register(s.id().to_owned(), s) }

    pub fn add_loader(&mut self, l: Box<dyn ThemeLoader>) { self.loaders.push(l) }

    pub fn resolve(&self, r: &ThemeRef) -> Result<Arc<dyn ThemeSource>, ThemeError> {
        if let Some(s) = self.sources.get(r.as_str()) { return Ok(s) }
        let loader = self.loaders.iter().find(|l| l.can_load(r));
        loader.ok_or_else(|| ThemeError::UnknownTheme(r.as_str().to_owned()))?.load(r)
    }

    /// The tokens `a` asks for in `preferred` lightness, with its overrides applied. An unknown
    /// or broken theme falls back to `system`; every failure is returned beside the tokens.
    pub fn tokens(&self, a: &Appearance, preferred: Variant) -> (TokenSet, Vec<ThemeError>) {
        let load = |s: Arc<dyn ThemeSource>| s.tokens(pick_variant(&*s, preferred));
        let (mut set, mut errors) = match self.resolve(&a.theme).and_then(load) {
            Ok(set) => (set, vec![]),
            Err(e) => ((*super::builtin::tokens(preferred)).clone(), vec![e]),
        };
        errors.extend(set.apply_table(&a.overrides));
        (set, errors)
    }

    /// `(id, label)` of every registered theme, for pickers.
    pub fn list(&self) -> Vec<(String, String)> { self.sources.iter().map(|(id, s)| (id.to_owned(), s.label().to_owned())).collect() }
}

/// The variant of `source` to show: `preferred` when offered, else its first.
pub fn pick_variant(source: &dyn ThemeSource, preferred: Variant) -> Variant {
    let offered = source.variants();
    if offered.contains(&preferred) { preferred } else { offered.first().copied().unwrap_or(preferred) }
}
