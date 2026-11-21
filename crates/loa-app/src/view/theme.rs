//! Appearance between the config schema and `loa_view::theme`.

use loa_settings::app as domain;
use loa_view::theme as view;

pub fn theme_to_view(theme: &domain::ThemeRef) -> view::ThemeRef { view::ThemeRef(theme.0.clone()) }

pub fn theme_from_view(theme: view::ThemeRef) -> domain::ThemeRef { domain::ThemeRef(theme.0) }

pub fn to_view(appearance: &domain::Appearance) -> view::Appearance {
    view::Appearance { theme: theme_to_view(&appearance.theme), overrides: appearance.overrides.clone() }
}

pub fn from_view(appearance: view::Appearance) -> domain::Appearance {
    domain::Appearance { theme: theme_from_view(appearance.theme), overrides: appearance.overrides }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_round_trips() {
        let overrides: toml::Table = toml::from_str("\"color.accent\" = \"#ff0000\"\n[space]\nm = 12\n").unwrap();
        let appearance = domain::Appearance { theme: domain::ThemeRef("/themes/neon.toml".into()), overrides };
        assert_eq!(from_view(to_view(&appearance)), appearance);
        assert_eq!(from_view(to_view(&domain::Appearance::default())), domain::Appearance::default());
    }
}
