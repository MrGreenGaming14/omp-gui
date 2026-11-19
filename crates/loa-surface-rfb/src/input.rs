//! Surface input to RFB PointerEvent and KeyEvent messages.

use crate::keysym::{self, ALT_L, CONTROL_L, SHIFT_L, SUPER_L};
use crate::{RfbError, proto};
use loa_surface::{Input, Modifiers, MouseButton};

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
                proto::pointer_event(buf, self.buttons, self.x, self.y);
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
                proto::pointer_event(buf, self.buttons, self.x, self.y);
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
                proto::pointer_event(buf, self.buttons, self.x, self.y);
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
                    proto::key_event(buf, *pressed, sym);
                } else {
                    let extra = self.extra(modifiers);
                    press(buf, &extra);
                    proto::key_event(buf, *pressed, sym);
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
                    proto::key_event(buf, true, sym);
                    proto::key_event(buf, false, sym);
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
            proto::pointer_event(buf, self.buttons | bit, self.x, self.y);
            proto::pointer_event(buf, self.buttons, self.x, self.y);
        }
    }
}

fn press(buf: &mut Vec<u8>, syms: &[Option<u32>]) {
    for &sym in syms.iter().flatten() {
        proto::key_event(buf, true, sym);
    }
}

fn release(buf: &mut Vec<u8>, syms: &[Option<u32>]) {
    for &sym in syms.iter().rev().flatten() {
        proto::key_event(buf, false, sym);
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

    fn encode(state: &mut InputState, input: Input) -> Vec<u8> {
        let mut buf = Vec::new();
        state.encode(&input, 800, 600, 40.0, &mut buf).unwrap();
        buf
    }

    fn ptr(mask: u8, x: u16, y: u16) -> Vec<u8> {
        let mut v = vec![5, mask];
        v.extend_from_slice(&x.to_be_bytes());
        v.extend_from_slice(&y.to_be_bytes());
        v
    }

    fn key(down: bool, sym: u32) -> Vec<u8> {
        let mut v = vec![4, u8::from(down), 0, 0];
        v.extend_from_slice(&sym.to_be_bytes());
        v
    }

    #[test]
    fn button_mask_tracks_held_buttons() {
        let mut s = InputState::default();
        let none = Modifiers::default();
        let b = |button, pressed| Input::MouseButton {
            x: 10.6,
            y: 20.2,
            button,
            pressed,
            modifiers: none,
        };
        assert_eq!(encode(&mut s, b(MouseButton::Left, true)), ptr(1, 10, 20));
        assert_eq!(encode(&mut s, b(MouseButton::Right, true)), ptr(5, 10, 20));
        assert_eq!(
            encode(&mut s, Input::MouseMove { x: 30.0, y: 40.0 }),
            ptr(5, 30, 40)
        );
        assert_eq!(encode(&mut s, b(MouseButton::Left, false)), ptr(4, 10, 20));
        assert_eq!(encode(&mut s, b(MouseButton::Middle, true)), ptr(6, 10, 20));
        assert_eq!(
            encode(&mut s, b(MouseButton::Back, true)),
            ptr(0x86, 10, 20)
        );
        let mut buf = Vec::new();
        assert!(matches!(
            s.encode(&b(MouseButton::Forward, true), 800, 600, 40.0, &mut buf),
            Err(RfbError::ForwardButton)
        ));
    }

    #[test]
    fn coordinates_clamp_to_the_framebuffer() {
        let mut s = InputState::default();
        assert_eq!(
            encode(&mut s, Input::MouseMove { x: -5.0, y: 9000.0 }),
            ptr(0, 0, 599)
        );
        assert_eq!(
            encode(
                &mut s,
                Input::MouseMove {
                    x: f32::NAN,
                    y: 599.9
                }
            ),
            ptr(0, 0, 599)
        );
    }

    #[test]
    fn wheel_clicks_accumulate_pixels() {
        let mut s = InputState::default();
        let none = Modifiers::default();
        let scroll = |dx, dy| Input::Scroll {
            x: 5.0,
            y: 6.0,
            dx,
            dy,
            modifiers: none,
        };
        // 100 px down: two clicks of button 5, 20 px carried over.
        let mut want = ptr(0, 5, 6);
        for _ in 0..2 {
            want.extend(ptr(WHEEL_DOWN, 5, 6));
            want.extend(ptr(0, 5, 6));
        }
        assert_eq!(encode(&mut s, scroll(0.0, 100.0)), want);
        // 20 more px completes a third click.
        let mut want = ptr(0, 5, 6);
        want.extend(ptr(WHEEL_DOWN, 5, 6));
        want.extend(ptr(0, 5, 6));
        assert_eq!(encode(&mut s, scroll(0.0, 20.0)), want);
        // Up is button 4, left button 6, right button 7.
        let mut want = ptr(0, 5, 6);
        want.extend(ptr(WHEEL_UP, 5, 6));
        want.extend(ptr(0, 5, 6));
        want.extend(ptr(WHEEL_LEFT, 5, 6));
        want.extend(ptr(0, 5, 6));
        assert_eq!(encode(&mut s, scroll(-40.0, -40.0)), want);
        let mut want = ptr(0, 5, 6);
        want.extend(ptr(WHEEL_RIGHT, 5, 6));
        want.extend(ptr(0, 5, 6));
        assert_eq!(encode(&mut s, scroll(40.0, 0.0)), want);
    }

    #[test]
    fn scroll_keeps_held_buttons() {
        let mut s = InputState::default();
        let none = Modifiers::default();
        encode(
            &mut s,
            Input::MouseButton {
                x: 1.0,
                y: 1.0,
                button: MouseButton::Left,
                pressed: true,
                modifiers: none,
            },
        );
        let mut want = ptr(1, 1, 1);
        want.extend(ptr(1 | WHEEL_DOWN, 1, 1));
        want.extend(ptr(1, 1, 1));
        assert_eq!(
            encode(
                &mut s,
                Input::Scroll {
                    x: 1.0,
                    y: 1.0,
                    dx: 0.0,
                    dy: 40.0,
                    modifiers: none
                }
            ),
            want
        );
    }

    #[test]
    fn modifiers_not_held_are_wrapped_around_the_event() {
        let mut s = InputState::default();
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::default()
        };
        let mut want = key(true, SHIFT_L);
        want.extend(key(true, CONTROL_L));
        want.extend(ptr(1, 3, 4));
        want.extend(key(false, CONTROL_L));
        want.extend(key(false, SHIFT_L));
        let click = Input::MouseButton {
            x: 3.0,
            y: 4.0,
            button: MouseButton::Left,
            pressed: true,
            modifiers: ctrl_shift,
        };
        assert_eq!(encode(&mut s, click), want);

        // Once Control is held through its own key event, only Shift is wrapped.
        let ctrl = |pressed| Input::Key {
            key: "Control".into(),
            code: "ControlLeft".into(),
            pressed,
            modifiers: ctrl_shift,
        };
        assert_eq!(encode(&mut s, ctrl(true)), key(true, CONTROL_L));
        let a = Input::Key {
            key: "A".into(),
            code: "KeyA".into(),
            pressed: true,
            modifiers: ctrl_shift,
        };
        let mut want = key(true, SHIFT_L);
        want.extend(key(true, 0x41));
        want.extend(key(false, SHIFT_L));
        assert_eq!(encode(&mut s, a), want);
        assert_eq!(encode(&mut s, ctrl(false)), key(false, CONTROL_L));
    }

    #[test]
    fn text_is_typed_key_by_key() {
        let mut s = InputState::default();
        let mut want = Vec::new();
        for sym in [0x68, 0xe9, 0x0100_20ac, keysym::RETURN, keysym::TAB] {
            want.extend(key(true, sym));
            want.extend(key(false, sym));
        }
        assert_eq!(
            encode(
                &mut s,
                Input::Text {
                    text: "hé€\r\n\t".into()
                }
            ),
            want
        );
    }

    #[test]
    fn unmappable_input_is_rejected() {
        let mut s = InputState::default();
        let mut buf = Vec::new();
        let dead = Input::Key {
            key: "Dead".into(),
            code: "Quote".into(),
            pressed: true,
            modifiers: Modifiers::default(),
        };
        assert!(
            matches!(s.encode(&dead, 10, 10, 40.0, &mut buf), Err(RfbError::Key(k)) if k == "Dead")
        );
        let bell = Input::Text {
            text: "\u{7}".into(),
        };
        assert!(matches!(
            s.encode(&bell, 10, 10, 40.0, &mut buf),
            Err(RfbError::Key(_))
        ));
    }
}
