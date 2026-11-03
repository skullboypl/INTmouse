//! VK ↔ portable [`Key`] mapping (Windows).

use inputshare_protocol::Key;

pub fn vk_to_key(vk: u32) -> Option<Key> {
    match vk {
        0x1B => Some(Key::Escape),
        0x09 => Some(Key::Tab),
        0x14 => Some(Key::CapsLock),
        0x90 => Some(Key::NumLock),
        0x91 => Some(Key::ScrollLock),
        0x10 | 0xA0 | 0xA1 => Some(Key::Shift),
        0x11 | 0xA2 | 0xA3 => Some(Key::Ctrl),
        0x12 | 0xA4 | 0xA5 => Some(Key::Alt),
        0x5B | 0x5C => Some(Key::Super),
        0x0D => Some(Key::Enter),
        0x08 => Some(Key::Backspace),
        0x20 => Some(Key::Space),
        0x25 => Some(Key::Left),
        0x27 => Some(Key::Right),
        0x26 => Some(Key::Up),
        0x28 => Some(Key::Down),
        0x2D => Some(Key::Insert),
        0x2E => Some(Key::Delete),
        0x24 => Some(Key::Home),
        0x23 => Some(Key::End),
        0x21 => Some(Key::PageUp),
        0x22 => Some(Key::PageDown),
        0x70..=0x7B => Some(Key::F((vk - 0x6F) as u8)),
        0x30..=0x39 => Some(Key::Char(char::from_u32(vk)?)),
        0x41..=0x5A => Some(Key::Char(char::from_u32(vk)?)),
        // OEM punctuation — best-effort as Unknown for cross-layout safety.
        other if other < 256 => Some(Key::Unknown(other)),
        _ => None,
    }
}

pub fn key_to_vk(key: Key) -> Option<u16> {
    match key {
        Key::Escape => Some(0x1B),
        Key::Tab => Some(0x09),
        Key::CapsLock => Some(0x14),
        Key::NumLock => Some(0x90),
        Key::ScrollLock => Some(0x91),
        Key::Shift => Some(0x10),
        Key::Ctrl => Some(0x11),
        Key::Alt => Some(0x12),
        Key::Super => Some(0x5B),
        Key::Enter => Some(0x0D),
        Key::Backspace => Some(0x08),
        Key::Space => Some(0x20),
        Key::Left => Some(0x25),
        Key::Right => Some(0x27),
        Key::Up => Some(0x26),
        Key::Down => Some(0x28),
        Key::Insert => Some(0x2D),
        Key::Delete => Some(0x2E),
        Key::Home => Some(0x24),
        Key::End => Some(0x23),
        Key::PageUp => Some(0x21),
        Key::PageDown => Some(0x22),
        Key::F(n) if (1..=12).contains(&n) => Some(0x6F + n as u16),
        Key::Char(c) => {
            let u = c.to_ascii_uppercase() as u32;
            if (0x30..=0x39).contains(&u) || (0x41..=0x5A).contains(&u) {
                Some(u as u16)
            } else {
                None
            }
        }
        Key::Unknown(v) if v < 256 => Some(v as u16),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_map_from_vk() {
        assert_eq!(vk_to_key(0x1B), Some(Key::Escape));
        assert_eq!(vk_to_key(0x09), Some(Key::Tab));
        assert_eq!(vk_to_key(0x0D), Some(Key::Enter));
        assert_eq!(vk_to_key(0x08), Some(Key::Backspace));
        assert_eq!(vk_to_key(0x20), Some(Key::Space));
        assert_eq!(vk_to_key(0x25), Some(Key::Left));
        assert_eq!(vk_to_key(0x26), Some(Key::Up));
        assert_eq!(vk_to_key(0x27), Some(Key::Right));
        assert_eq!(vk_to_key(0x28), Some(Key::Down));
        assert_eq!(vk_to_key(0x2D), Some(Key::Insert));
        assert_eq!(vk_to_key(0x2E), Some(Key::Delete));
        assert_eq!(vk_to_key(0x24), Some(Key::Home));
        assert_eq!(vk_to_key(0x23), Some(Key::End));
        assert_eq!(vk_to_key(0x21), Some(Key::PageUp));
        assert_eq!(vk_to_key(0x22), Some(Key::PageDown));
        assert_eq!(vk_to_key(0x14), Some(Key::CapsLock));
        assert_eq!(vk_to_key(0x90), Some(Key::NumLock));
        assert_eq!(vk_to_key(0x91), Some(Key::ScrollLock));
    }

    #[test]
    fn left_right_modifiers_collapse_to_one_portable_key() {
        for vk in [0x10, 0xA0, 0xA1] {
            assert_eq!(vk_to_key(vk), Some(Key::Shift), "vk {vk:#x}");
        }
        for vk in [0x11, 0xA2, 0xA3] {
            assert_eq!(vk_to_key(vk), Some(Key::Ctrl), "vk {vk:#x}");
        }
        for vk in [0x12, 0xA4, 0xA5] {
            assert_eq!(vk_to_key(vk), Some(Key::Alt), "vk {vk:#x}");
        }
        assert_eq!(vk_to_key(0x5B), Some(Key::Super));
        assert_eq!(vk_to_key(0x5C), Some(Key::Super));
    }

    #[test]
    fn function_keys_f1_to_f12() {
        assert_eq!(vk_to_key(0x70), Some(Key::F(1)));
        assert_eq!(vk_to_key(0x7B), Some(Key::F(12)));
        // F13 is outside the portable range: kept as an opaque VK.
        assert_eq!(vk_to_key(0x7C), Some(Key::Unknown(0x7C)));
    }

    #[test]
    fn digits_and_letters_become_uppercase_chars() {
        assert_eq!(vk_to_key(0x30), Some(Key::Char('0')));
        assert_eq!(vk_to_key(0x39), Some(Key::Char('9')));
        assert_eq!(vk_to_key(0x41), Some(Key::Char('A')));
        assert_eq!(vk_to_key(0x5A), Some(Key::Char('Z')));
    }

    #[test]
    fn oem_and_numpad_keys_are_unknown_but_preserved() {
        assert_eq!(vk_to_key(0xBA), Some(Key::Unknown(0xBA))); // OEM_1
        assert_eq!(vk_to_key(0x60), Some(Key::Unknown(0x60))); // NUMPAD0
    }

    #[test]
    fn vk_out_of_byte_range_is_rejected() {
        assert_eq!(vk_to_key(256), None);
        assert_eq!(vk_to_key(0xFFFF), None);
        assert_eq!(vk_to_key(u32::MAX), None);
    }

    #[test]
    fn key_to_vk_named_keys() {
        assert_eq!(key_to_vk(Key::Escape), Some(0x1B));
        assert_eq!(key_to_vk(Key::Shift), Some(0x10));
        assert_eq!(key_to_vk(Key::Ctrl), Some(0x11));
        assert_eq!(key_to_vk(Key::Alt), Some(0x12));
        assert_eq!(key_to_vk(Key::Super), Some(0x5B));
        assert_eq!(key_to_vk(Key::Enter), Some(0x0D));
        assert_eq!(key_to_vk(Key::PageDown), Some(0x22));
        assert_eq!(key_to_vk(Key::CapsLock), Some(0x14));
    }

    #[test]
    fn key_to_vk_function_keys_only_1_to_12() {
        assert_eq!(key_to_vk(Key::F(1)), Some(0x70));
        assert_eq!(key_to_vk(Key::F(12)), Some(0x7B));
        assert_eq!(key_to_vk(Key::F(0)), None);
        assert_eq!(key_to_vk(Key::F(13)), None);
    }

    #[test]
    fn key_to_vk_chars_are_case_insensitive_alnum_only() {
        assert_eq!(key_to_vk(Key::Char('a')), Some(0x41));
        assert_eq!(key_to_vk(Key::Char('A')), Some(0x41));
        assert_eq!(key_to_vk(Key::Char('7')), Some(0x37));
        assert_eq!(key_to_vk(Key::Char('!')), None);
        assert_eq!(key_to_vk(Key::Char(' ')), None);
        assert_eq!(key_to_vk(Key::Char('ż')), None);
    }

    #[test]
    fn key_to_vk_unknown_only_below_256() {
        assert_eq!(key_to_vk(Key::Unknown(0xBA)), Some(0xBA));
        assert_eq!(key_to_vk(Key::Unknown(255)), Some(255));
        assert_eq!(key_to_vk(Key::Unknown(256)), None);
    }

    #[test]
    fn portable_key_survives_vk_roundtrip() {
        // vk -> key -> vk -> key must be stable (the wire only carries `Key`).
        for vk in 0u32..256 {
            let Some(key) = vk_to_key(vk) else { continue };
            let back = key_to_vk(key).unwrap_or_else(|| panic!("no vk for {key:?} (from {vk:#x})"));
            assert_eq!(vk_to_key(back as u32), Some(key), "vk {vk:#x} via {key:?}");
        }
    }

    #[test]
    fn every_canonical_key_roundtrips_exactly() {
        let keys = [
            Key::Escape,
            Key::Tab,
            Key::CapsLock,
            Key::NumLock,
            Key::ScrollLock,
            Key::Shift,
            Key::Ctrl,
            Key::Alt,
            Key::Super,
            Key::Enter,
            Key::Backspace,
            Key::Space,
            Key::Left,
            Key::Right,
            Key::Up,
            Key::Down,
            Key::Insert,
            Key::Delete,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::F(1),
            Key::F(6),
            Key::F(12),
            Key::Char('Q'),
            Key::Char('5'),
            Key::Unknown(0xDE),
        ];
        for k in keys {
            let vk = key_to_vk(k).unwrap_or_else(|| panic!("no vk for {k:?}"));
            assert_eq!(vk_to_key(vk as u32), Some(k), "{k:?}");
        }
    }
}
