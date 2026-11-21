//! Translation of surface input into CDP `Input.*` commands.

use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventPointerType,
    DispatchMouseEventType, InsertTextParams, MouseButton as CdpButton,
};
use chromiumoxide::keys::{USKEYBOARD_LAYOUT, get_key_definition};
use loa_contracts::surface::{Input, Modifiers, MouseButton};
use std::time::{Duration, Instant};

/// Presses closer than this in time and space count as one multi-click.
const MULTI_CLICK_TIME: Duration = Duration::from_millis(500);
const MULTI_CLICK_DISTANCE: f32 = 4.0;

/// Largest surface edge a viewer may ask for.
pub(crate) const MAX_EDGE: u32 = 8192;

/// Each button with its CDP name and its bit of the CDP `buttons` field, in bit order.
const BUTTONS: [(MouseButton, CdpButton, i64); 5] = [
    (MouseButton::Left, CdpButton::Left, 1),
    (MouseButton::Right, CdpButton::Right, 2),
    (MouseButton::Middle, CdpButton::Middle, 4),
    (MouseButton::Back, CdpButton::Back, 8),
    (MouseButton::Forward, CdpButton::Forward, 16),
];

/// One CDP call for one surface input.
#[derive(Debug, PartialEq)]
pub(crate) enum Command {
    Mouse(DispatchMouseEventParams),
    Key(DispatchKeyEventParams),
    Text(InsertTextParams),
    Resize { width: u32, height: u32 },
}

/// Pointer state CDP needs on every mouse event but the contract sends once.
#[derive(Default)]
pub(crate) struct InputMapper {
    /// CDP `buttons` bit mask of the held buttons.
    held: i64,
    last_press: Option<(MouseButton, Instant, f32, f32)>,
    click_count: i64,
}

impl InputMapper {
    /// The command for `input`, or `None` when it has no effect.
    pub fn map(&mut self, input: &Input, now: Instant) -> Option<Command> {
        let command = match *input {
            Input::MouseMove { x, y } => {
                // CDP wants one `button` on moves: the first held one, else none.
                let held = BUTTONS.iter().find(|(_, _, bit)| self.held & bit != 0);
                let mut params = mouse(DispatchMouseEventType::MouseMoved, x, y, self.held, None);
                params.button = Some(held.map_or(CdpButton::None, |(_, cdp, _)| cdp.clone()));
                Command::Mouse(params)
            }
            Input::MouseButton { x, y, button, pressed, modifiers } => {
                let (_, cdp, bit) = BUTTONS.iter().find(|(b, _, _)| *b == button).expect("every button is listed");
                if pressed {
                    let near = |(last, at, lx, ly): (MouseButton, Instant, f32, f32)| {
                        let close = (x - lx).abs() <= MULTI_CLICK_DISTANCE && (y - ly).abs() <= MULTI_CLICK_DISTANCE;
                        last == button && now.duration_since(at) <= MULTI_CLICK_TIME && close
                    };
                    self.click_count = if self.last_press.is_some_and(near) { self.click_count + 1 } else { 1 };
                    self.last_press = Some((button, now, x, y));
                    self.held |= bit;
                } else {
                    self.held &= !bit;
                }
                let kind = if pressed { DispatchMouseEventType::MousePressed } else { DispatchMouseEventType::MouseReleased };
                let mut params = mouse(kind, x, y, self.held, Some(modifiers));
                params.button = Some(cdp.clone());
                params.click_count = Some(self.click_count.max(1));
                Command::Mouse(params)
            }
            Input::Scroll { x, y, dx, dy, modifiers } => {
                let mut params = mouse(DispatchMouseEventType::MouseWheel, x, y, self.held, Some(modifiers));
                (params.delta_x, params.delta_y) = (Some(f64::from(dx)), Some(f64::from(dy)));
                Command::Mouse(params)
            }
            Input::Key { ref key, ref code, pressed, modifiers } => Command::Key(key_event(key, code, pressed, modifiers)),
            Input::Text { ref text } if text.is_empty() => return None,
            Input::Text { ref text } => Command::Text(InsertTextParams::new(text.clone())),
            Input::Resize { width, height } => {
                Command::Resize { width: width.clamp(1, MAX_EDGE), height: height.clamp(1, MAX_EDGE) }
            }
        };
        Some(command)
    }
}

fn mouse(kind: DispatchMouseEventType, x: f32, y: f32, held: i64, modifiers: Option<Modifiers>) -> DispatchMouseEventParams {
    let mut params = DispatchMouseEventParams::new(kind, f64::from(x), f64::from(y));
    params.pointer_type = Some(DispatchMouseEventPointerType::Mouse);
    params.buttons = Some(held);
    params.modifiers = modifiers.map(modifier_bits);
    params
}

/// Bits of the CDP `modifiers` field.
fn modifier_bits(modifiers: Modifiers) -> i64 {
    i64::from(modifiers.alt) | i64::from(modifiers.ctrl) << 1 | i64::from(modifiers.meta) << 2 | i64::from(modifiers.shift) << 3
}

/// Key presses carry no text: committed text arrives as `Input::Text` and is
/// inserted with `Input.insertText`. The exception is keys whose default
/// action is a control character (Enter), which Chrome only performs on a
/// key event with text.
fn key_event(key: &str, code: &str, pressed: bool, modifiers: Modifiers) -> DispatchKeyEventParams {
    let definition = get_key_definition(key)
        .filter(|d| code.is_empty() || d.code == code)
        .or_else(|| USKEYBOARD_LAYOUT.iter().find(|d| !code.is_empty() && d.code == code));
    let control_text = definition.and_then(|d| d.text).filter(|t| t.chars().all(char::is_control));
    let kind = match (pressed, control_text) {
        (false, _) => DispatchKeyEventType::KeyUp,
        (true, Some(_)) => DispatchKeyEventType::KeyDown,
        (true, None) => DispatchKeyEventType::RawKeyDown,
    };
    let mut params = DispatchKeyEventParams::new(kind);
    params.key = Some(key.to_owned());
    params.code = Some(if code.is_empty() { definition.map_or("", |d| d.code).to_owned() } else { code.to_owned() });
    params.windows_virtual_key_code = definition.map(|d| d.key_code);
    params.native_virtual_key_code = definition.map(|d| d.key_code);
    params.modifiers = Some(modifier_bits(modifiers));
    if pressed && let Some(text) = control_text {
        params.text = Some(text.to_owned());
        params.unmodified_text = Some(text.to_owned());
    }
    let sided = definition.is_some_and(|d| ["Shift", "Control", "Alt", "Meta"].contains(&d.key));
    params.location = match code {
        _ if code.starts_with("Numpad") => Some(3),
        _ if sided && code.ends_with("Right") => Some(2),
        _ if sided && code.ends_with("Left") => Some(1),
        _ => None,
    };
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_events_count_clicks_carry_held_buttons_and_modifiers() {
        let mut mapper = InputMapper::default();
        let t0 = Instant::now();
        let mut mouse = |input: Input, at: Duration| match mapper.map(&input, t0 + at) {
            Some(Command::Mouse(p)) => (p.r#type, p.button, p.buttons, p.click_count, p.modifiers),
            other => panic!("{other:?}"),
        };
        let none = Modifiers::default();
        let left = |x, pressed| Input::MouseButton { x, y: 10.0, button: MouseButton::Left, pressed, modifiers: none };
        let ms = Duration::from_millis;
        let (press, release, moved) =
            (DispatchMouseEventType::MousePressed, DispatchMouseEventType::MouseReleased, DispatchMouseEventType::MouseMoved);
        let l = Some(CdpButton::Left);
        // A quick second press in place is a double click; a late or far one is not.
        let steps = [
            (left(10.0, true), ms(0), (press.clone(), l.clone(), Some(1), Some(1), Some(0))),
            (Input::MouseMove { x: 50.0, y: 60.0 }, ms(0), (moved.clone(), l.clone(), Some(1), None, None)),
            (left(10.0, false), ms(0), (release.clone(), l.clone(), Some(0), Some(1), Some(0))),
            (Input::MouseMove { x: 51.0, y: 60.0 }, ms(0), (moved, Some(CdpButton::None), Some(0), None, None)),
            (left(11.0, true), ms(120), (press.clone(), l.clone(), Some(1), Some(2), Some(0))),
            (left(11.0, false), ms(120), (release.clone(), l.clone(), Some(0), Some(2), Some(0))),
            (left(11.0, true), ms(1020), (press.clone(), l.clone(), Some(1), Some(1), Some(0))),
            (left(11.0, false), ms(1020), (release, l.clone(), Some(0), Some(1), Some(0))),
            (left(80.0, true), ms(1030), (press, l, Some(1), Some(1), Some(0))),
        ];
        for (n, (input, at, want)) in steps.into_iter().enumerate() {
            assert_eq!(mouse(input, at), want, "step {n}");
        }
        let modifiers = Modifiers { ctrl: true, shift: true, ..none };
        let input = Input::Scroll { x: 5.0, y: 6.0, dx: -3.0, dy: 120.0, modifiers };
        let Some(Command::Mouse(wheel)) = mapper.map(&input, t0) else { panic!() };
        assert_eq!((wheel.r#type, wheel.delta_x, wheel.delta_y, wheel.modifiers), (DispatchMouseEventType::MouseWheel, Some(-3.0), Some(120.0), Some(2 | 8)));
        assert_eq!(mapper.map(&Input::Text { text: String::new() }, t0), None);
        let resize = mapper.map(&Input::Resize { width: 0, height: 100_000 }, t0);
        assert_eq!(resize, Some(Command::Resize { width: 1, height: MAX_EDGE }));
    }

    #[test]
    fn keys_get_virtual_key_codes_and_only_control_keys_carry_text() {
        let none = Modifiers::default();
        let ctrl = Modifiers { ctrl: true, ..none };
        let shift = Modifiers { shift: true, ..none };
        let (raw, down, up) = (DispatchKeyEventType::RawKeyDown, DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp);
        let cases = [
            (("Backspace", "Backspace", true, none), (raw.clone(), Some(8), None, Some(0))),
            (("Enter", "Enter", true, none), (down, Some(13), Some("\r"), Some(0))),
            (("Enter", "Enter", false, none), (up, Some(13), None, Some(0))),
            // Letters insert no text twice; select all keeps its Ctrl.
            (("a", "KeyA", true, ctrl), (raw.clone(), Some(65), None, Some(2))),
            // A shifted layout key is still found by its physical code.
            (("!", "Digit1", true, shift), (raw, Some(49), None, Some(8))),
        ];
        for ((key, code, pressed, modifiers), want) in cases {
            let e = key_event(key, code, pressed, modifiers);
            assert_eq!((e.r#type, e.windows_virtual_key_code, e.text.as_deref(), e.modifiers), want, "{key}");
        }
    }
}
