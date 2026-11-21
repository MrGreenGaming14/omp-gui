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
                let mut params = mouse(DispatchMouseEventType::MouseMoved, x, y);
                params.button = Some(self.held_button());
                params.buttons = Some(self.held);
                Command::Mouse(params)
            }
            Input::MouseButton { x, y, button, pressed, modifiers } => {
                if pressed {
                    self.click_count = match self.last_press {
                        Some((last, at, lx, ly))
                            if last == button
                                && now.duration_since(at) <= MULTI_CLICK_TIME
                                && (x - lx).abs() <= MULTI_CLICK_DISTANCE
                                && (y - ly).abs() <= MULTI_CLICK_DISTANCE =>
                        {
                            self.click_count + 1
                        }
                        _ => 1,
                    };
                    self.last_press = Some((button, now, x, y));
                    self.held |= button_bit(button);
                } else {
                    self.held &= !button_bit(button);
                }
                let kind = if pressed { DispatchMouseEventType::MousePressed } else { DispatchMouseEventType::MouseReleased };
                let mut params = mouse(kind, x, y);
                params.button = Some(cdp_button(button));
                params.buttons = Some(self.held);
                params.click_count = Some(self.click_count.max(1));
                params.modifiers = Some(modifier_bits(modifiers));
                Command::Mouse(params)
            }
            Input::Scroll { x, y, dx, dy, modifiers } => {
                let mut params = mouse(DispatchMouseEventType::MouseWheel, x, y);
                params.delta_x = Some(f64::from(dx));
                params.delta_y = Some(f64::from(dy));
                params.buttons = Some(self.held);
                params.modifiers = Some(modifier_bits(modifiers));
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

    /// CDP wants one `button` on moves: the first held one, else none.
    fn held_button(&self) -> CdpButton {
        [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward]
            .into_iter()
            .find(|&b| self.held & button_bit(b) != 0)
            .map_or(CdpButton::None, cdp_button)
    }
}

fn mouse(kind: DispatchMouseEventType, x: f32, y: f32) -> DispatchMouseEventParams {
    let mut params = DispatchMouseEventParams::new(kind, f64::from(x), f64::from(y));
    params.pointer_type = Some(DispatchMouseEventPointerType::Mouse);
    params
}

fn cdp_button(button: MouseButton) -> CdpButton {
    match button {
        MouseButton::Left => CdpButton::Left,
        MouseButton::Middle => CdpButton::Middle,
        MouseButton::Right => CdpButton::Right,
        MouseButton::Back => CdpButton::Back,
        MouseButton::Forward => CdpButton::Forward,
    }
}

/// Bits of the CDP `buttons` field.
fn button_bit(button: MouseButton) -> i64 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        MouseButton::Back => 8,
        MouseButton::Forward => 16,
    }
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
    if code.starts_with("Numpad") {
        params.location = Some(3);
    } else if code.ends_with("Right") && definition.is_some_and(|d| ["Shift", "Control", "Alt", "Meta"].contains(&d.key)) {
        params.location = Some(2);
    } else if code.ends_with("Left") && definition.is_some_and(|d| ["Shift", "Control", "Alt", "Meta"].contains(&d.key)) {
        params.location = Some(1);
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(mapper: &mut InputMapper, x: f32, at: Instant) -> DispatchMouseEventParams {
        let input = Input::MouseButton { x, y: 10.0, button: MouseButton::Left, pressed: true, modifiers: Modifiers::default() };
        match mapper.map(&input, at) {
            Some(Command::Mouse(p)) => p,
            other => panic!("{other:?}"),
        }
    }

    fn release(mapper: &mut InputMapper, x: f32, at: Instant) -> DispatchMouseEventParams {
        let input = Input::MouseButton { x, y: 10.0, button: MouseButton::Left, pressed: false, modifiers: Modifiers::default() };
        match mapper.map(&input, at) {
            Some(Command::Mouse(p)) => p,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quick_second_press_in_place_is_a_double_click_but_a_far_or_late_one_is_not() {
        let mut mapper = InputMapper::default();
        let t0 = Instant::now();
        assert_eq!(press(&mut mapper, 10.0, t0).click_count, Some(1));
        assert_eq!(release(&mut mapper, 10.0, t0).click_count, Some(1));
        let t1 = t0 + Duration::from_millis(120);
        assert_eq!(press(&mut mapper, 11.0, t1).click_count, Some(2));
        assert_eq!(release(&mut mapper, 11.0, t1).click_count, Some(2));
        let t2 = t1 + Duration::from_millis(900);
        assert_eq!(press(&mut mapper, 11.0, t2).click_count, Some(1));
        release(&mut mapper, 11.0, t2);
        assert_eq!(press(&mut mapper, 80.0, t2 + Duration::from_millis(10)).click_count, Some(1));
    }

    #[test]
    fn drag_moves_carry_the_held_button() {
        let mut mapper = InputMapper::default();
        let now = Instant::now();
        press(&mut mapper, 1.0, now);
        let Some(Command::Mouse(moved)) = mapper.map(&Input::MouseMove { x: 50.0, y: 60.0 }, now) else { panic!() };
        assert_eq!((moved.button, moved.buttons), (Some(CdpButton::Left), Some(1)));
        release(&mut mapper, 50.0, now);
        let Some(Command::Mouse(moved)) = mapper.map(&Input::MouseMove { x: 51.0, y: 60.0 }, now) else { panic!() };
        assert_eq!((moved.button, moved.buttons), (Some(CdpButton::None), Some(0)));
    }

    #[test]
    fn wheel_keeps_direction_and_modifiers() {
        let mut mapper = InputMapper::default();
        let modifiers = Modifiers { ctrl: true, shift: true, ..Modifiers::default() };
        let input = Input::Scroll { x: 5.0, y: 6.0, dx: -3.0, dy: 120.0, modifiers };
        let Some(Command::Mouse(wheel)) = mapper.map(&input, Instant::now()) else { panic!() };
        assert_eq!(wheel.r#type, DispatchMouseEventType::MouseWheel);
        assert_eq!((wheel.delta_x, wheel.delta_y), (Some(-3.0), Some(120.0)));
        assert_eq!(wheel.modifiers, Some(2 | 8));
    }

    #[test]
    fn editing_keys_get_virtual_key_codes_and_enter_carries_its_text() {
        let backspace = key_event("Backspace", "Backspace", true, Modifiers::default());
        assert_eq!(backspace.r#type, DispatchKeyEventType::RawKeyDown);
        assert_eq!(backspace.windows_virtual_key_code, Some(8));
        assert_eq!(backspace.text, None);

        let enter = key_event("Enter", "Enter", true, Modifiers::default());
        assert_eq!(enter.r#type, DispatchKeyEventType::KeyDown);
        assert_eq!(enter.text.as_deref(), Some("\r"));
        assert_eq!(enter.windows_virtual_key_code, Some(13));
        let enter_up = key_event("Enter", "Enter", false, Modifiers::default());
        assert_eq!((enter_up.r#type, enter_up.text), (DispatchKeyEventType::KeyUp, None));
    }

    #[test]
    fn letter_keys_do_not_insert_text_twice_and_select_all_keeps_ctrl() {
        let a = key_event("a", "KeyA", true, Modifiers { ctrl: true, ..Modifiers::default() });
        assert_eq!(a.r#type, DispatchKeyEventType::RawKeyDown);
        assert_eq!(a.text, None);
        assert_eq!(a.windows_virtual_key_code, Some(65));
        assert_eq!(a.modifiers, Some(2));
        // A shifted layout key is still found by its physical code.
        let bang = key_event("!", "Digit1", true, Modifiers { shift: true, ..Modifiers::default() });
        assert_eq!(bang.windows_virtual_key_code, Some(49));
    }

    #[test]
    fn empty_text_is_dropped_and_resize_is_bounded() {
        let mut mapper = InputMapper::default();
        assert_eq!(mapper.map(&Input::Text { text: String::new() }, Instant::now()), None);
        let resize = mapper.map(&Input::Resize { width: 0, height: 100_000 }, Instant::now());
        assert_eq!(resize, Some(Command::Resize { width: 1, height: MAX_EDGE }));
    }
}
