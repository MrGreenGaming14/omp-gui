//! Surface input to RFB PointerEvent and KeyEvent messages.

use crate::vnc::keysym::{self, ALT_L, CONTROL_L, SHIFT_L, SUPER_L};
use crate::vnc::RfbError;
use crate::vnc::proto::Msg;
use loa_contracts::surface::{Input, Modifiers, MouseButton};

const WHEEL_UP: u8 = 1 << 3;
const WHEEL_DOWN: u8 = 1 << 4;
const WHEEL_LEFT: u8 = 1 << 5;
const WHEEL_RIGHT: u8 = 1 << 6;

/// What the server has been told so far: pressed buttons, pointer position,
/// held modifiers, and scroll distance not yet sent as a wheel click.
#[derive(Debug, Default)]
pub(crate) struct InputState {
    buttons: u8,
    x: u16,
    y: u16,
    held: Modifiers,
    scroll_x: f32,
    scroll_y: f32,
}

impl InputState {
    /// Forgets pressed buttons and keys, for a fresh connection.
    pub fn reset(&mut self) {
        *self = Self {
            x: self.x,
            y: self.y,
            ..Self::default()
        };
    }

    /// Appends the messages for `input` to `buf`. `width` and `height` are the
    /// framebuffer size, `scroll_step` the pixels per wheel click. On error
    /// `buf` may hold a partial encoding and must not be sent.
    pub fn encode(
        &mut self,
        input: &Input,
        width: u32,
        height: u32,
        scroll_step: f32,
        buf: &mut Vec<u8>,
    ) -> Result<(), RfbError> {
        match input {
            Input::MouseMove { x, y } => {
                self.move_to(*x, *y, width, height);
                Msg::Pointer { mask: self.buttons, x: self.x, y: self.y }.write(buf);
            }
            Input::MouseButton {
                x,
                y,
                button,
                pressed,
                modifiers,
            } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 1 << 1,
                    MouseButton::Right => 1 << 2,
                    MouseButton::Back => 1 << 7,
                    MouseButton::Forward => return Err(RfbError::ForwardButton),
                };
                let extra = self.extra(modifiers);
                press(buf, &extra);
                self.move_to(*x, *y, width, height);
                if *pressed {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                }
                Msg::Pointer { mask: self.buttons, x: self.x, y: self.y }.write(buf);
                release(buf, &extra);
            }
            Input::Scroll {
                x,
                y,
                dx,
                dy,
                modifiers,
            } => {
                let extra = self.extra(modifiers);
                press(buf, &extra);
                self.move_to(*x, *y, width, height);
                Msg::Pointer { mask: self.buttons, x: self.x, y: self.y }.write(buf);
                let clicks_y = take_clicks(&mut self.scroll_y, *dy, scroll_step);
                self.wheel(buf, clicks_y, WHEEL_DOWN, WHEEL_UP);
                let clicks_x = take_clicks(&mut self.scroll_x, *dx, scroll_step);
                self.wheel(buf, clicks_x, WHEEL_RIGHT, WHEEL_LEFT);
                release(buf, &extra);
            }
            Input::Key {
                key,
                code,
                pressed,
                modifiers,
            } => {
                let sym = keysym::keysym(key, code).ok_or_else(|| RfbError::Key(key.clone()))?;
                if keysym::is_modifier(key) {
                    let flag = match key.as_str() {
                        "Shift" => &mut self.held.shift,
                        "Control" => &mut self.held.ctrl,
                        "Alt" => &mut self.held.alt,
                        _ => &mut self.held.meta,
                    };
                    *flag = *pressed;
                    Msg::Key { down: *pressed, keysym: sym }.write(buf);
                } else {
                    let extra = self.extra(modifiers);
                    press(buf, &extra);
                    Msg::Key { down: *pressed, keysym: sym }.write(buf);
                    release(buf, &extra);
                }
            }
            Input::Text { text } => {
                let mut chars = text.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        continue;
                    }
                    let sym = keysym::char_keysym(c).ok_or_else(|| RfbError::Key(c.to_string()))?;
                    Msg::Key { down: true, keysym: sym }.write(buf);
                    Msg::Key { down: false, keysym: sym }.write(buf);
                }
            }
            // RFB 3.8 has no client initiated resize without the
            // ExtendedDesktopSize extension; the surface keeps its size.
            Input::Resize { .. } => {}
        }
        Ok(())
    }

    fn move_to(&mut self, x: f32, y: f32, width: u32, height: u32) {
        self.x = coordinate(x, width);
        self.y = coordinate(y, height);
    }

    /// Modifier keysyms `wanted` asks for that are not held down.
    fn extra(&self, wanted: &Modifiers) -> [Option<u32>; 4] {
        let held = &self.held;
        [
            (wanted.shift && !held.shift).then_some(SHIFT_L),
            (wanted.ctrl && !held.ctrl).then_some(CONTROL_L),
            (wanted.alt && !held.alt).then_some(ALT_L),
            (wanted.meta && !held.meta).then_some(SUPER_L),
        ]
    }

    /// Clicks the wheel button `positive` (or `negative` for negative
    /// counts) `clicks` times at the current position.
    fn wheel(&self, buf: &mut Vec<u8>, clicks: i32, positive: u8, negative: u8) {
        let bit = if clicks > 0 { positive } else { negative };
        for _ in 0..clicks.unsigned_abs() {
            Msg::Pointer { mask: self.buttons | bit, x: self.x, y: self.y }.write(buf);
            Msg::Pointer { mask: self.buttons, x: self.x, y: self.y }.write(buf);
        }
    }
}

fn press(buf: &mut Vec<u8>, syms: &[Option<u32>]) {
    for &sym in syms.iter().flatten() {
        Msg::Key { down: true, keysym: sym }.write(buf);
    }
}

fn release(buf: &mut Vec<u8>, syms: &[Option<u32>]) {
    for &sym in syms.iter().rev().flatten() {
        Msg::Key { down: false, keysym: sym }.write(buf);
    }
}

/// Surface coordinate to a framebuffer pixel, clamped to the framebuffer.
fn coordinate(v: f32, size: u32) -> u16 {
    let max = size.saturating_sub(1).min(u32::from(u16::MAX)) as f32;
    // Truncation is the pixel the point lies in; NaN becomes 0.
    v.clamp(0.0, max) as u16
}

/// Adds `delta` to the accumulator and takes whole wheel clicks out of it.
fn take_clicks(acc: &mut f32, delta: f32, step: f32) -> i32 {
    if !delta.is_finite() || step <= 0.0 {
        return 0;
    }
    *acc += delta;
    let clicks = (*acc / step).trunc();
    *acc -= clicks * step;
    clicks as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ptr(mask: u8, x: u16, y: u16) -> Vec<u8> {
        Msg::Pointer { mask, x, y }.bytes()
    }

    fn key(down: bool, keysym: u32) -> Vec<u8> {
        Msg::Key { down, keysym }.bytes()
    }

    #[test]
    fn inputs_become_pointer_and_key_events() {
        let none = Modifiers::default();
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..none };
        let button = |button, pressed, modifiers| Input::MouseButton { x: 10.6, y: 20.2, button, pressed, modifiers };
        let scroll = |dx, dy| Input::Scroll { x: 5.0, y: 6.0, dx, dy, modifiers: none };
        let key_input = |key: &str, code: &str, pressed| Input::Key { key: key.into(), code: code.into(), pressed, modifiers: ctrl_shift };
        let wheel = |held: u8, bits: &[u8]| {
            let clicks = bits.iter().flat_map(|bit| [ptr(held | bit, 5, 6), ptr(held, 5, 6)]);
            [ptr(held, 5, 6)].into_iter().chain(clicks).collect::<Vec<_>>().concat()
        };
        let typed = [0x68, 0xe9, 0x0100_20ac, keysym::RETURN, keysym::TAB].into_iter().flat_map(|s| [key(true, s), key(false, s)]);
        // One state through the whole sequence: buttons, wheel remainders and held modifiers carry over.
        let steps: Vec<(Input, Vec<u8>)> = vec![
            (button(MouseButton::Left, true, none), ptr(1, 10, 20)),
            (button(MouseButton::Right, true, none), ptr(5, 10, 20)),
            (Input::MouseMove { x: 30.0, y: 40.0 }, ptr(5, 30, 40)),
            (button(MouseButton::Left, false, none), ptr(4, 10, 20)),
            (button(MouseButton::Back, true, none), ptr(0x84, 10, 20)),
            (button(MouseButton::Back, false, none), ptr(4, 10, 20)),
            (button(MouseButton::Right, false, none), ptr(0, 10, 20)),
            // Coordinates clamp to the framebuffer; NaN is 0.
            (Input::MouseMove { x: -5.0, y: 9000.0 }, ptr(0, 0, 599)),
            (Input::MouseMove { x: f32::NAN, y: 599.9 }, ptr(0, 0, 599)),
            // 100 px down is two clicks with 20 px carried over; 20 more completes a third.
            (scroll(0.0, 100.0), wheel(0, &[WHEEL_DOWN, WHEEL_DOWN])),
            (scroll(0.0, 20.0), wheel(0, &[WHEEL_DOWN])),
            (scroll(-40.0, -40.0), wheel(0, &[WHEEL_UP, WHEEL_LEFT])),
            (scroll(40.0, 0.0), wheel(0, &[WHEEL_RIGHT])),
            // Modifiers not held are pressed around the event and released in reverse.
            (button(MouseButton::Left, true, ctrl_shift), [key(true, SHIFT_L), key(true, CONTROL_L), ptr(1, 10, 20), key(false, CONTROL_L), key(false, SHIFT_L)].concat()),
            (Input::Scroll { x: 5.0, y: 6.0, dx: 0.0, dy: 40.0, modifiers: none }, wheel(1, &[WHEEL_DOWN])),
            // Once Control is held through its own key event, only Shift is wrapped.
            (key_input("Control", "ControlLeft", true), key(true, CONTROL_L)),
            (key_input("A", "KeyA", true), [key(true, SHIFT_L), key(true, 0x41), key(false, SHIFT_L)].concat()),
            (key_input("Control", "ControlLeft", false), key(false, CONTROL_L)),
            (Input::Text { text: "hé€\r\n\t".into() }, typed.collect::<Vec<_>>().concat()),
            (Input::Resize { width: 1, height: 1 }, Vec::new()),
        ];
        let mut state = InputState::default();
        for (n, (input, want)) in steps.into_iter().enumerate() {
            let mut buf = Vec::new();
            state.encode(&input, 800, 600, 40.0, &mut buf).unwrap();
            assert_eq!(buf, want, "step {n}: {input:?}");
        }
        let dead = Input::Key { key: "Dead".into(), code: "Quote".into(), pressed: true, modifiers: none };
        for input in [dead, Input::Text { text: "\u{7}".into() }, button(MouseButton::Forward, true, none)] {
            let error = state.encode(&input, 10, 10, 40.0, &mut Vec::new()).unwrap_err();
            assert!(matches!(error, RfbError::Key(_) | RfbError::ForwardButton), "{error}");
        }
    }
}

