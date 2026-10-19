//! Shared foreground dismissal gestures preserve composition and the current keyboard layout.

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

use super::MACOS_ESCAPE_KEY_CODE;

/// Inspect a main-thread key event without copying input strings or owning its responder/session.
pub(super) fn is_dismissal_event(event: &NSEvent, marked_text: bool) -> bool {
    if event.r#type() != NSEventType::KeyDown {
        // Character getters are meaningful only for key events; mouse/device events stay with AppKit.
        return false;
    }
    let modifiers = semantic_modifiers(event.modifierFlags());
    let character = if modifiers == NSEventModifierFlags::Command {
        // Command-W follows the event's logical character rather than assuming the ANSI W key's position.
        match event.charactersIgnoringModifiers() {
            Some(characters) if characters.length() == 1 => {
                // Only one bounded UTF-16 unit can identify this ASCII command, including Caps Lock's W.
                Some(characters.characterAtIndex(0))
            }
            Some(_) | None => {
                // Missing, surrogate-pair or multi-character input cannot alias the declared W command.
                None
            }
        }
    } else {
        // Ordinary typing, Escape and modified navigation need no character materialization here.
        None
    };
    matches_dismissal(event.keyCode(), modifiers, marked_text, character)
}

/// Ignore device/Caps Lock flags, matching the baseline's four semantic modifier coordinates.
fn semantic_modifiers(modifiers: NSEventModifierFlags) -> NSEventModifierFlags {
    modifiers.intersection(
        NSEventModifierFlags::Command
            | NSEventModifierFlags::Control
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Shift,
    )
}

/// Pure policy distinguishes input-method Escape from an explicit fallible Command-W close request.
fn matches_dismissal(
    key_code: u16,
    modifiers: NSEventModifierFlags,
    marked_text: bool,
    character: Option<u16>,
) -> bool {
    let modifiers = semantic_modifiers(modifiers);
    if key_code == MACOS_ESCAPE_KEY_CODE {
        // Physical Escape belongs to the panel only without modifiers and after composition has ended.
        return modifiers.is_empty() && !marked_text;
    }
    // An exact logical Command-W uses the existing save/resignation preflight, even during composition.
    modifiers == NSEventModifierFlags::Command && matches!(character, Some(0x57 | 0x77))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise every semantic chord and layout coordinate without constructing native events or UI.
    #[test]
    fn dismissal_preserves_modifiers_composition_and_logical_w() {
        let bits = [
            NSEventModifierFlags::Command,
            NSEventModifierFlags::Control,
            NSEventModifierFlags::Option,
            NSEventModifierFlags::Shift,
        ];
        for mask in 0_u8..16 {
            // Cover all sixteen modifier combinations, including mixed chords that must remain native input.
            let mut modifiers = NSEventModifierFlags::empty();
            for (index, flag) in bits.iter().enumerate() {
                if mask & (1 << index) != 0 {
                    // Each selected coordinate matches the NSEvent flag read by both foreground adapters.
                    modifiers |= *flag;
                }
            }
            for marked in [false, true] {
                // Input-method ownership changes Escape admission but cannot bypass Command-W's close preflight.
                assert_eq!(
                    matches_dismissal(53, modifiers, marked, None),
                    mask == 0 && !marked
                );
                for physical_key in [0, 6, 13] {
                    // Logical W can occupy another physical key under a different keyboard layout.
                    assert_eq!(
                        matches_dismissal(physical_key, modifiers, marked, Some(0x77)),
                        mask == 1
                    );
                    assert_eq!(
                        matches_dismissal(physical_key, modifiers, marked, Some(0x57)),
                        mask == 1
                    );
                }
                for character in [None, Some(0x78), Some(0x1b)] {
                    // The ANSI W position alone, another letter, or an Escape character cannot fabricate Command-W.
                    assert!(!matches_dismissal(13, modifiers, marked, character));
                }
            }
        }
        // Caps Lock is not an extra semantic modifier and does not hide either ordinary supported gesture.
        assert!(matches_dismissal(
            53,
            NSEventModifierFlags::CapsLock,
            false,
            None
        ));
        assert!(matches_dismissal(
            0,
            NSEventModifierFlags::Command | NSEventModifierFlags::CapsLock,
            true,
            Some(0x57)
        ));
    }
}
