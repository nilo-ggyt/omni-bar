//! Historical key names intersected with public HIToolbox virtual-key definitions.
//!
//! The scope is global-hotkey 0.8.0's parser/Carbon map plus the installed public Events.h.
//! Media event taps, unmapped function keys and the undocumented PrintScreen code are excluded.

/// Static key facts are shared by Copy chords; no runtime key registry or heap cache exists.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct ShortcutKey {
    pub(super) name: &'static str,
    /// Presentation only; name, aliases, and the Carbon code still determine parsing, persistence, and registration.
    display_label: &'static str,
    pub(super) code: u32,
    aliases: &'static [&'static str],
}

impl ShortcutKey {
    /// Return this static key's Chinese name or keycap text without feeding the label back into parsing.
    pub(super) fn label(&self) -> &'static str {
        // Letters, digits, and function keys keep their keycaps; other keys use the table's explicit Chinese names.
        self.display_label
    }
}

/// Look up complete historical aliases; unknown text never becomes a raw numeric native code.
pub(super) fn find(name: &str) -> Option<&'static ShortcutKey> {
    // This finite source table bounds each explicit parse independently of user input length.
    KEYS.iter().find(|key| {
        // ASCII case folding matches the baseline's key-name grammar without a persistent normalized string.
        key.aliases
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

/// Keep a recognized but unsupported old key distinct from a malformed saved preference.
pub(super) fn known_without_public_mapping(name: &str) -> bool {
    // These parser names cannot enter the existing Carbon-only adapter; no event tap is introduced.
    UNSUPPORTED
        .iter()
        .any(|alias| alias.eq_ignore_ascii_case(name))
}

// Each factual row pairs baseline names with its corroborated public Carbon virtual key value.
pub(super) const KEYS: &[ShortcutKey] = &[
    ShortcutKey {
        name: "Backquote",
        display_label: "`",
        code: 0x32,
        aliases: &["BACKQUOTE", "`"],
    },
    ShortcutKey {
        name: "Backslash",
        display_label: "\\",
        code: 0x2a,
        aliases: &["BACKSLASH", "\\"],
    },
    ShortcutKey {
        name: "BracketLeft",
        display_label: "[",
        code: 0x21,
        aliases: &["BRACKETLEFT", "["],
    },
    ShortcutKey {
        name: "BracketRight",
        display_label: "]",
        code: 0x1e,
        aliases: &["BRACKETRIGHT", "]"],
    },
    ShortcutKey {
        name: "Comma",
        display_label: ",",
        code: 0x2b,
        aliases: &["COMMA", ","],
    },
    ShortcutKey {
        name: "Digit0",
        display_label: "0",
        code: 0x1d,
        aliases: &["DIGIT0", "0"],
    },
    ShortcutKey {
        name: "Digit1",
        display_label: "1",
        code: 0x12,
        aliases: &["DIGIT1", "1"],
    },
    ShortcutKey {
        name: "Digit2",
        display_label: "2",
        code: 0x13,
        aliases: &["DIGIT2", "2"],
    },
    ShortcutKey {
        name: "Digit3",
        display_label: "3",
        code: 0x14,
        aliases: &["DIGIT3", "3"],
    },
    ShortcutKey {
        name: "Digit4",
        display_label: "4",
        code: 0x15,
        aliases: &["DIGIT4", "4"],
    },
    ShortcutKey {
        name: "Digit5",
        display_label: "5",
        code: 0x17,
        aliases: &["DIGIT5", "5"],
    },
    ShortcutKey {
        name: "Digit6",
        display_label: "6",
        code: 0x16,
        aliases: &["DIGIT6", "6"],
    },
    ShortcutKey {
        name: "Digit7",
        display_label: "7",
        code: 0x1a,
        aliases: &["DIGIT7", "7"],
    },
    ShortcutKey {
        name: "Digit8",
        display_label: "8",
        code: 0x1c,
        aliases: &["DIGIT8", "8"],
    },
    ShortcutKey {
        name: "Digit9",
        display_label: "9",
        code: 0x19,
        aliases: &["DIGIT9", "9"],
    },
    ShortcutKey {
        name: "Equal",
        display_label: "=",
        code: 0x18,
        aliases: &["EQUAL", "="],
    },
    ShortcutKey {
        name: "KeyA",
        display_label: "A",
        code: 0x00,
        aliases: &["KEYA", "A"],
    },
    ShortcutKey {
        name: "KeyB",
        display_label: "B",
        code: 0x0b,
        aliases: &["KEYB", "B"],
    },
    ShortcutKey {
        name: "KeyC",
        display_label: "C",
        code: 0x08,
        aliases: &["KEYC", "C"],
    },
    ShortcutKey {
        name: "KeyD",
        display_label: "D",
        code: 0x02,
        aliases: &["KEYD", "D"],
    },
    ShortcutKey {
        name: "KeyE",
        display_label: "E",
        code: 0x0e,
        aliases: &["KEYE", "E"],
    },
    ShortcutKey {
        name: "KeyF",
        display_label: "F",
        code: 0x03,
        aliases: &["KEYF", "F"],
    },
    ShortcutKey {
        name: "KeyG",
        display_label: "G",
        code: 0x05,
        aliases: &["KEYG", "G"],
    },
    ShortcutKey {
        name: "KeyH",
        display_label: "H",
        code: 0x04,
        aliases: &["KEYH", "H"],
    },
    ShortcutKey {
        name: "KeyI",
        display_label: "I",
        code: 0x22,
        aliases: &["KEYI", "I"],
    },
    ShortcutKey {
        name: "KeyJ",
        display_label: "J",
        code: 0x26,
        aliases: &["KEYJ", "J"],
    },
    ShortcutKey {
        name: "KeyK",
        display_label: "K",
        code: 0x28,
        aliases: &["KEYK", "K"],
    },
    ShortcutKey {
        name: "KeyL",
        display_label: "L",
        code: 0x25,
        aliases: &["KEYL", "L"],
    },
    ShortcutKey {
        name: "KeyM",
        display_label: "M",
        code: 0x2e,
        aliases: &["KEYM", "M"],
    },
    ShortcutKey {
        name: "KeyN",
        display_label: "N",
        code: 0x2d,
        aliases: &["KEYN", "N"],
    },
    ShortcutKey {
        name: "KeyO",
        display_label: "O",
        code: 0x1f,
        aliases: &["KEYO", "O"],
    },
    ShortcutKey {
        name: "KeyP",
        display_label: "P",
        code: 0x23,
        aliases: &["KEYP", "P"],
    },
    ShortcutKey {
        name: "KeyQ",
        display_label: "Q",
        code: 0x0c,
        aliases: &["KEYQ", "Q"],
    },
    ShortcutKey {
        name: "KeyR",
        display_label: "R",
        code: 0x0f,
        aliases: &["KEYR", "R"],
    },
    ShortcutKey {
        name: "KeyS",
        display_label: "S",
        code: 0x01,
        aliases: &["KEYS", "S"],
    },
    ShortcutKey {
        name: "KeyT",
        display_label: "T",
        code: 0x11,
        aliases: &["KEYT", "T"],
    },
    ShortcutKey {
        name: "KeyU",
        display_label: "U",
        code: 0x20,
        aliases: &["KEYU", "U"],
    },
    ShortcutKey {
        name: "KeyV",
        display_label: "V",
        code: 0x09,
        aliases: &["KEYV", "V"],
    },
    ShortcutKey {
        name: "KeyW",
        display_label: "W",
        code: 0x0d,
        aliases: &["KEYW", "W"],
    },
    ShortcutKey {
        name: "KeyX",
        display_label: "X",
        code: 0x07,
        aliases: &["KEYX", "X"],
    },
    ShortcutKey {
        name: "KeyY",
        display_label: "Y",
        code: 0x10,
        aliases: &["KEYY", "Y"],
    },
    ShortcutKey {
        name: "KeyZ",
        display_label: "Z",
        code: 0x06,
        aliases: &["KEYZ", "Z"],
    },
    ShortcutKey {
        name: "Minus",
        display_label: "-",
        code: 0x1b,
        aliases: &["MINUS", "-"],
    },
    ShortcutKey {
        name: "Period",
        display_label: ".",
        code: 0x2f,
        aliases: &["PERIOD", "."],
    },
    ShortcutKey {
        name: "Quote",
        display_label: "'",
        code: 0x27,
        aliases: &["QUOTE", "'"],
    },
    ShortcutKey {
        name: "Semicolon",
        display_label: ";",
        code: 0x29,
        aliases: &["SEMICOLON", ";"],
    },
    ShortcutKey {
        name: "Slash",
        display_label: "/",
        code: 0x2c,
        aliases: &["SLASH", "/"],
    },
    ShortcutKey {
        name: "Backspace",
        display_label: "退格",
        code: 0x33,
        aliases: &["BACKSPACE"],
    },
    ShortcutKey {
        name: "CapsLock",
        display_label: "大写锁定",
        code: 0x39,
        aliases: &["CAPSLOCK"],
    },
    ShortcutKey {
        name: "Enter",
        display_label: "回车",
        code: 0x24,
        aliases: &["ENTER"],
    },
    ShortcutKey {
        name: "Space",
        display_label: "空格",
        code: 0x31,
        aliases: &["SPACE"],
    },
    ShortcutKey {
        name: "Tab",
        display_label: "制表",
        code: 0x30,
        aliases: &["TAB"],
    },
    ShortcutKey {
        name: "Delete",
        display_label: "向前删除",
        code: 0x75,
        aliases: &["DELETE"],
    },
    ShortcutKey {
        name: "End",
        display_label: "文末",
        code: 0x77,
        aliases: &["END"],
    },
    ShortcutKey {
        name: "Home",
        display_label: "文首",
        code: 0x73,
        aliases: &["HOME"],
    },
    ShortcutKey {
        name: "Insert",
        display_label: "插入",
        code: 0x72,
        aliases: &["INSERT"],
    },
    ShortcutKey {
        name: "PageDown",
        display_label: "下翻页",
        code: 0x79,
        aliases: &["PAGEDOWN"],
    },
    ShortcutKey {
        name: "PageUp",
        display_label: "上翻页",
        code: 0x74,
        aliases: &["PAGEUP"],
    },
    ShortcutKey {
        name: "ArrowDown",
        display_label: "↓",
        code: 0x7d,
        aliases: &["ARROWDOWN", "DOWN"],
    },
    ShortcutKey {
        name: "ArrowLeft",
        display_label: "←",
        code: 0x7b,
        aliases: &["ARROWLEFT", "LEFT"],
    },
    ShortcutKey {
        name: "ArrowRight",
        display_label: "→",
        code: 0x7c,
        aliases: &["ARROWRIGHT", "RIGHT"],
    },
    ShortcutKey {
        name: "ArrowUp",
        display_label: "↑",
        code: 0x7e,
        aliases: &["ARROWUP", "UP"],
    },
    ShortcutKey {
        name: "NumLock",
        display_label: "数字锁定",
        code: 0x47,
        aliases: &["NUMLOCK"],
    },
    ShortcutKey {
        name: "Numpad0",
        display_label: "数字键盘0",
        code: 0x52,
        aliases: &["NUMPAD0", "NUM0"],
    },
    ShortcutKey {
        name: "Numpad1",
        display_label: "数字键盘1",
        code: 0x53,
        aliases: &["NUMPAD1", "NUM1"],
    },
    ShortcutKey {
        name: "Numpad2",
        display_label: "数字键盘2",
        code: 0x54,
        aliases: &["NUMPAD2", "NUM2"],
    },
    ShortcutKey {
        name: "Numpad3",
        display_label: "数字键盘3",
        code: 0x55,
        aliases: &["NUMPAD3", "NUM3"],
    },
    ShortcutKey {
        name: "Numpad4",
        display_label: "数字键盘4",
        code: 0x56,
        aliases: &["NUMPAD4", "NUM4"],
    },
    ShortcutKey {
        name: "Numpad5",
        display_label: "数字键盘5",
        code: 0x57,
        aliases: &["NUMPAD5", "NUM5"],
    },
    ShortcutKey {
        name: "Numpad6",
        display_label: "数字键盘6",
        code: 0x58,
        aliases: &["NUMPAD6", "NUM6"],
    },
    ShortcutKey {
        name: "Numpad7",
        display_label: "数字键盘7",
        code: 0x59,
        aliases: &["NUMPAD7", "NUM7"],
    },
    ShortcutKey {
        name: "Numpad8",
        display_label: "数字键盘8",
        code: 0x5b,
        aliases: &["NUMPAD8", "NUM8"],
    },
    ShortcutKey {
        name: "Numpad9",
        display_label: "数字键盘9",
        code: 0x5c,
        aliases: &["NUMPAD9", "NUM9"],
    },
    ShortcutKey {
        name: "NumpadAdd",
        display_label: "数字键盘＋",
        code: 0x45,
        aliases: &["NUMPADADD", "NUMADD", "NUMPADPLUS", "NUMPLUS"],
    },
    ShortcutKey {
        name: "NumpadDecimal",
        display_label: "数字键盘小数点",
        code: 0x41,
        aliases: &["NUMPADDECIMAL", "NUMDECIMAL"],
    },
    ShortcutKey {
        name: "NumpadDivide",
        display_label: "数字键盘÷",
        code: 0x4b,
        aliases: &["NUMPADDIVIDE", "NUMDIVIDE"],
    },
    ShortcutKey {
        name: "NumpadEnter",
        display_label: "数字键盘回车",
        code: 0x4c,
        aliases: &["NUMPADENTER", "NUMENTER"],
    },
    ShortcutKey {
        name: "NumpadEqual",
        display_label: "数字键盘＝",
        code: 0x51,
        aliases: &["NUMPADEQUAL", "NUMEQUAL"],
    },
    ShortcutKey {
        name: "NumpadMultiply",
        display_label: "数字键盘×",
        code: 0x43,
        aliases: &["NUMPADMULTIPLY", "NUMMULTIPLY"],
    },
    ShortcutKey {
        name: "NumpadSubtract",
        display_label: "数字键盘－",
        code: 0x4e,
        aliases: &["NUMPADSUBTRACT", "NUMSUBTRACT"],
    },
    ShortcutKey {
        name: "Escape",
        display_label: "退出",
        code: 0x35,
        aliases: &["ESCAPE", "ESC"],
    },
    ShortcutKey {
        name: "F1",
        display_label: "F1",
        code: 0x7a,
        aliases: &["F1"],
    },
    ShortcutKey {
        name: "F2",
        display_label: "F2",
        code: 0x78,
        aliases: &["F2"],
    },
    ShortcutKey {
        name: "F3",
        display_label: "F3",
        code: 0x63,
        aliases: &["F3"],
    },
    ShortcutKey {
        name: "F4",
        display_label: "F4",
        code: 0x76,
        aliases: &["F4"],
    },
    ShortcutKey {
        name: "F5",
        display_label: "F5",
        code: 0x60,
        aliases: &["F5"],
    },
    ShortcutKey {
        name: "F6",
        display_label: "F6",
        code: 0x61,
        aliases: &["F6"],
    },
    ShortcutKey {
        name: "F7",
        display_label: "F7",
        code: 0x62,
        aliases: &["F7"],
    },
    ShortcutKey {
        name: "F8",
        display_label: "F8",
        code: 0x64,
        aliases: &["F8"],
    },
    ShortcutKey {
        name: "F9",
        display_label: "F9",
        code: 0x65,
        aliases: &["F9"],
    },
    ShortcutKey {
        name: "F10",
        display_label: "F10",
        code: 0x6d,
        aliases: &["F10"],
    },
    ShortcutKey {
        name: "F11",
        display_label: "F11",
        code: 0x67,
        aliases: &["F11"],
    },
    ShortcutKey {
        name: "F12",
        display_label: "F12",
        code: 0x6f,
        aliases: &["F12"],
    },
    ShortcutKey {
        name: "AudioVolumeDown",
        display_label: "降低音量",
        code: 0x49,
        aliases: &["AUDIOVOLUMEDOWN", "VOLUMEDOWN"],
    },
    ShortcutKey {
        name: "AudioVolumeUp",
        display_label: "提高音量",
        code: 0x48,
        aliases: &["AUDIOVOLUMEUP", "VOLUMEUP"],
    },
    ShortcutKey {
        name: "AudioVolumeMute",
        display_label: "静音",
        code: 0x4a,
        aliases: &["AUDIOVOLUMEMUTE", "VOLUMEMUTE"],
    },
    ShortcutKey {
        name: "F13",
        display_label: "F13",
        code: 0x69,
        aliases: &["F13"],
    },
    ShortcutKey {
        name: "F14",
        display_label: "F14",
        code: 0x6b,
        aliases: &["F14"],
    },
    ShortcutKey {
        name: "F15",
        display_label: "F15",
        code: 0x71,
        aliases: &["F15"],
    },
    ShortcutKey {
        name: "F16",
        display_label: "F16",
        code: 0x6a,
        aliases: &["F16"],
    },
    ShortcutKey {
        name: "F17",
        display_label: "F17",
        code: 0x40,
        aliases: &["F17"],
    },
    ShortcutKey {
        name: "F18",
        display_label: "F18",
        code: 0x4f,
        aliases: &["F18"],
    },
    ShortcutKey {
        name: "F19",
        display_label: "F19",
        code: 0x50,
        aliases: &["F19"],
    },
    ShortcutKey {
        name: "F20",
        display_label: "F20",
        code: 0x5a,
        aliases: &["F20"],
    },
];

// An explicit warning preserves these old settings without claiming their keys can be registered.
const UNSUPPORTED: &[&str] = &[
    "PAUSE",
    "PAUSEBREAK",
    "PRINTSCREEN",
    "SCROLLLOCK",
    "MEDIAPLAY",
    "MEDIAPAUSE",
    "MEDIAPLAYPAUSE",
    "MEDIASTOP",
    "MEDIATRACKNEXT",
    "MEDIATRACKPREV",
    "MEDIATRACKPREVIOUS",
    "F21",
    "F22",
    "F23",
    "F24",
];
