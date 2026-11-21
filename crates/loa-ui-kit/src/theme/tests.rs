use super::*;
use crate::style::{self, ButtonKind};
use iced::widget::button;

fn builtin(v: Variant) -> TokenSet { ThemeRegistry::builtin().resolve(&ThemeRef(v.as_str().into())).unwrap().tokens(v).unwrap() }

#[test]
fn text_roles_meet_wcag_aa_on_every_ground_of_both_variants() {
    use ColorRole::*;
    let pairs = [(Text, Ground), (Text2, Ground), (Text3, Ground), (Warn, Ground), (Text, Side), (Text2, Side), (Text3, Side), (Warn, Side), (Text, Card), (Text2, Card), (Text3, Card), (OnAccent, Accent), (Side, Text)];
    for v in Variant::ALL {
        let t = builtin(*v);
        for (fg, bg) in pairs {
            let c = contrast(t.color(fg), t.color(bg));
            assert!(c >= 4.5, "{v}: {fg} on {bg} is {c:.2}");
        }
    }
}

#[test]
fn role_names_round_trip_and_index_their_tables() {
    fn check<R: Role + PartialEq + std::fmt::Debug + std::fmt::Display>(all: &[R]) {
        all.iter().for_each(|&r| assert_eq!(R::parse(&r.to_string()), Some(r)));
    }
    check(ColorRole::ALL);
    check(TextRole::ALL);
    check(SpaceRole::ALL);
    check(RadiusRole::ALL);
    check(AlphaRole::ALL);
    check(LayoutRole::ALL);
    assert_eq!((ColorRole::term(0), ColorRole::term(15)), (ColorRole::Term0, ColorRole::Term15));
    let t = builtin(Variant::Dark);
    assert_eq!(t.palette(), iced::theme::Palette { background: t.color(ColorRole::Ground), ..t.palette() });
    assert_eq!(t.font(TextRole::Mono), iced::Font::MONOSPACE);
}

#[test]
fn overrides_parse_apply_and_report_failures() {
    let mut t = builtin(Variant::Dark);
    let table: toml::Table = toml::from_str(
        r##"
        "color.accent" = "#ff000080"
        color.line = "text.2"
        color.hover = "accent @ 0.5"
        space.m = 20
        text.body = 15.5
        style.button.ghost.hover.bg = "danger @ tint"
        style.button.ghost.base.radius = "pill"
        color.nope = "#000000"
        space.m2 = 1
        layout.nav = "wide"
        style.button.ghost.hover.bg2 = "accent"
        flag = true
        "##,
    )
    .unwrap();
    let errors: Vec<String> = t.apply_table(&table).iter().map(ToString::to_string).collect();
    assert_eq!(t.color(ColorRole::Accent), iced::Color::from_rgba8(255, 0, 0, 128.0 / 255.0));
    assert_eq!(t.color(ColorRole::Line), t.color(ColorRole::Text2));
    assert_eq!(t.color(ColorRole::Hover), t.color(ColorRole::Accent).scale_alpha(0.5));
    assert_eq!((t.space(SpaceRole::M), t.text(TextRole::Body)), (20.0, 15.5));
    let ghost = t.look(ButtonKind::Ghost.key(), StyleState::Hover);
    assert_eq!(ghost.bg, Some(t.color(ColorRole::Danger).scale_alpha(0.35)));
    assert_eq!(ghost.border.radius, t.radius(RadiusRole::Pill).into());
    assert_eq!(errors.len(), 5, "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("color.nope")) && errors.iter().any(|e| e.contains("flag")));
}

#[test]
fn registry_resolves_builtins_lists_them_and_falls_back() {
    let r = ThemeRegistry::builtin();
    assert_eq!(r.list().iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["dark", "light", "system"]);
    let a = |name: &str| Appearance { theme: ThemeRef(name.into()), overrides: Default::default() };
    assert_eq!(r.tokens(&a("light"), Variant::Dark).0.variant, Variant::Light);
    assert_eq!(r.tokens(&a("system"), Variant::Light).0.variant, Variant::Light);
    let (set, errors) = r.tokens(&a("neon"), Variant::Dark);
    assert_eq!((set.variant, errors), (Variant::Dark, vec![ThemeError::UnknownTheme("neon".into())]));
}

#[test]
fn recipes_follow_the_active_set_and_states() {
    let mut t = builtin(Variant::Light);
    t.apply("color.selected", TokenValue::parse("#123456")).unwrap();
    let theme = t.theme();
    let before = generation();
    assert!(set_active(t.clone()) > before);
    let row = style::button(ButtonKind::Row { selected: true })(&theme, button::Status::Active);
    assert_eq!(row.background, Some(iced::Color::from_rgb8(0x12, 0x34, 0x56).into()));
    let ghost = |s| style::button(ButtonKind::Ghost)(&theme, s);
    assert_eq!((ghost(button::Status::Active).background, ghost(button::Status::Active).text_color), (None, t.color(ColorRole::Text2)));
    assert_eq!(ghost(button::Status::Pressed).background, Some(t.color(ColorRole::Hover).into()));
    let other = builtin(Variant::Dark).theme();
    assert_eq!(style::Tone::Normal.color(&other), builtin(Variant::Dark).color(ColorRole::Text), "a theme the active set did not build reads its builtin");
}
