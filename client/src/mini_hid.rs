//! JetKVM Mini HIDRPC wire frames.

use crate::hid_protocol::Event;

pub const HANDSHAKE: [u8; 2] = [0x01, 0x01];
pub const KEEPALIVE: [u8; 1] = [0x09];

pub fn keyboard(modifier: u8, keys: &[(u16, u16)]) -> Vec<u8> {
    let mut frame = vec![0x02, modifier];
    frame.extend(
        keys.iter()
            .filter(|(page, usage)| *page == 7 && *usage <= u16::from(u8::MAX))
            .take(6)
            .map(|(_, usage)| *usage as u8),
    );
    frame.resize(8, 0);
    frame
}

pub fn relative(dx: i16, dy: i16, buttons: u8) -> Vec<u8> {
    vec![
        0x06,
        dx.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8 as u8,
        dy.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8 as u8,
        buttons,
    ]
}

pub fn wheel(vertical: i16, horizontal: i16) -> Vec<u8> {
    vec![
        0x04,
        vertical.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8 as u8,
        horizontal.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8 as u8,
    ]
}

pub fn encode(event: &Event, buttons: u8) -> Option<Vec<u8>> {
    match event {
        Event::KeyboardState { modifiers, keys } => Some(keyboard(*modifiers, keys)),
        Event::MouseButtons { mask } => Some(relative(0, 0, *mask)),
        Event::Wheel {
            vertical,
            horizontal,
        } => Some(wheel(*vertical, *horizontal)),
        Event::Motion { dx, dy } => Some(relative(*dx, *dy, buttons)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_mini_wire_shapes() {
        assert_eq!(HANDSHAKE, [1, 1]);
        assert_eq!(KEEPALIVE, [9]);
        assert_eq!(keyboard(2, &[(7, 4), (7, 5)]), [2, 2, 4, 5, 0, 0, 0, 0]);
        assert_eq!(relative(-2, 3, 1), [6, 254, 3, 1]);
        assert_eq!(wheel(120, -120), [4, 120, 136]);
    }

    #[test]
    fn non_keyboard_pages_are_not_sent_as_keyboard_usage() {
        assert_eq!(keyboard(0, &[(1, 1)]), [2, 0, 0, 0, 0, 0, 0, 0]);
    }
}
