use crate::log_debug;
#[cfg(not(test))]
use crate::mouse::ffi::{
    self, CGEventCreateKeyboardEvent, CGEventGetFlags, CGEventPost, CGEventSetFlags,
    CGEventSourceCreate, CGEventSourceRef, K_CG_EVENT_SOURCE_STATE_PRIVATE, K_CG_HID_EVENT_TAP,
};
use crate::mouse::ffi::{
    CGEventFlags, K_CG_EVENT_FLAG_MASK_ALTERNATE, K_CG_EVENT_FLAG_MASK_COMMAND,
    K_CG_EVENT_FLAG_MASK_CONTROL, K_CG_EVENT_FLAG_MASK_SHIFT, K_NX_KEYTYPE_BRIGHTNESS_DOWN,
    K_NX_KEYTYPE_BRIGHTNESS_UP, K_NX_KEYTYPE_MUTE, K_NX_KEYTYPE_NEXT, K_NX_KEYTYPE_PLAY,
    K_NX_KEYTYPE_PREVIOUS, K_NX_KEYTYPE_SOUND_DOWN, K_NX_KEYTYPE_SOUND_UP, K_VK_ANSI_0,
    K_VK_ANSI_1, K_VK_ANSI_2, K_VK_ANSI_3, K_VK_ANSI_4, K_VK_ANSI_5, K_VK_ANSI_6, K_VK_ANSI_7,
    K_VK_ANSI_8, K_VK_ANSI_9, K_VK_ANSI_A, K_VK_ANSI_B, K_VK_ANSI_BACKSLASH, K_VK_ANSI_C,
    K_VK_ANSI_COMMA, K_VK_ANSI_D, K_VK_ANSI_E, K_VK_ANSI_EQUAL, K_VK_ANSI_F, K_VK_ANSI_G,
    K_VK_ANSI_GRAVE, K_VK_ANSI_H, K_VK_ANSI_I, K_VK_ANSI_J, K_VK_ANSI_K, K_VK_ANSI_L,
    K_VK_ANSI_LEFT_BRACKET, K_VK_ANSI_M, K_VK_ANSI_MINUS, K_VK_ANSI_N, K_VK_ANSI_O, K_VK_ANSI_P,
    K_VK_ANSI_PERIOD, K_VK_ANSI_Q, K_VK_ANSI_QUOTE, K_VK_ANSI_R, K_VK_ANSI_RIGHT_BRACKET,
    K_VK_ANSI_S, K_VK_ANSI_SEMICOLON, K_VK_ANSI_SLASH, K_VK_ANSI_T, K_VK_ANSI_U, K_VK_ANSI_V,
    K_VK_ANSI_W, K_VK_ANSI_X, K_VK_ANSI_Y, K_VK_ANSI_Z, K_VK_CAPS_LOCK, K_VK_COMMAND, K_VK_CONTROL,
    K_VK_DELETE, K_VK_DOWN_ARROW, K_VK_END, K_VK_ESCAPE, K_VK_FORWARD_DELETE, K_VK_HOME,
    K_VK_LAUNCHPAD, K_VK_LEFT_ARROW, K_VK_MISSION_CONTROL, K_VK_OPTION, K_VK_PAGE_DOWN,
    K_VK_PAGE_UP, K_VK_RETURN, K_VK_RIGHT_ARROW, K_VK_SHIFT, K_VK_SPACE, K_VK_TAB, K_VK_UP_ARROW,
};
#[cfg(not(test))]
use objc2::runtime::AnyObject;
#[cfg(not(test))]
use objc2::{class, sel};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PressedKey {

    pub modifiers: CGEventFlags,
    pub action: KeyAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyAction {

    Keyboard { keycode: u16 },

    Media { nx_key: i32 },
}

pub(crate) fn parse_action(raw: &str) -> Option<PressedKey> {
    let s = raw.trim();
    let lowercase;
    let s = if s.bytes().any(|b| b.is_ascii_uppercase()) {
        lowercase = s.to_ascii_lowercase();
        lowercase.as_str()
    } else {
        s
    };
    if s.is_empty() {
        return None;
    }
    let mut parts = s.split('+').map(str::trim);
    let last = parts.next_back()?;

    let mut modifiers: CGEventFlags = 0;
    let mut seen = [false; 4];
    for p in parts {
        let (flag, idx) = modifier(p)?;

        if seen[idx] {
            return None;
        }
        seen[idx] = true;
        modifiers |= flag;
    }

    if let Some((_, idx)) = modifier(last) {
        if seen[idx] {
            return None;
        }
        return Some(PressedKey {
            modifiers,
            action: KeyAction::Keyboard {
                keycode: modifier_keycode(last)?,
            },
        });
    }

    Some(PressedKey {
        modifiers,
        action: key_action(last)?,
    })
}

fn modifier(name: &str) -> Option<(CGEventFlags, usize)> {
    match name {
        "cmd" | "command" => Some((K_CG_EVENT_FLAG_MASK_COMMAND, 0)),
        "ctrl" | "control" => Some((K_CG_EVENT_FLAG_MASK_CONTROL, 1)),
        "opt" | "option" | "alt" => Some((K_CG_EVENT_FLAG_MASK_ALTERNATE, 2)),
        "shift" => Some((K_CG_EVENT_FLAG_MASK_SHIFT, 3)),
        _ => None,
    }
}

fn modifier_keycode(name: &str) -> Option<u16> {
    match name {
        "cmd" | "command" => Some(K_VK_COMMAND),
        "ctrl" | "control" => Some(K_VK_CONTROL),
        "opt" | "option" | "alt" => Some(K_VK_OPTION),
        "shift" => Some(K_VK_SHIFT),
        _ => None,
    }
}

fn key_action(name: &str) -> Option<KeyAction> {

    if name.len() == 1 {
        let c = name.chars().next().unwrap();
        if let Some(kc) = letter_keycode(c) {
            return Some(KeyAction::Keyboard { keycode: kc });
        }
        if let Some(kc) = digit_keycode(c) {
            return Some(KeyAction::Keyboard { keycode: kc });
        }
        if let Some(kc) = punctuation_keycode(c) {
            return Some(KeyAction::Keyboard { keycode: kc });
        }
    }

    match name {

        "left" | "left_arrow" | "arrow_left" => Some(KeyAction::Keyboard {
            keycode: K_VK_LEFT_ARROW,
        }),
        "right" | "right_arrow" | "arrow_right" => Some(KeyAction::Keyboard {
            keycode: K_VK_RIGHT_ARROW,
        }),
        "up" | "up_arrow" | "arrow_up" => Some(KeyAction::Keyboard {
            keycode: K_VK_UP_ARROW,
        }),
        "down" | "down_arrow" | "arrow_down" => Some(KeyAction::Keyboard {
            keycode: K_VK_DOWN_ARROW,
        }),

        "home" => Some(KeyAction::Keyboard { keycode: K_VK_HOME }),
        "end" => Some(KeyAction::Keyboard { keycode: K_VK_END }),
        "page_up" | "pageup" | "pgup" => Some(KeyAction::Keyboard {
            keycode: K_VK_PAGE_UP,
        }),
        "page_down" | "pagedown" | "pgdn" => Some(KeyAction::Keyboard {
            keycode: K_VK_PAGE_DOWN,
        }),
        "space" => Some(KeyAction::Keyboard {
            keycode: K_VK_SPACE,
        }),
        "return" | "enter" => Some(KeyAction::Keyboard {
            keycode: K_VK_RETURN,
        }),
        "tab" => Some(KeyAction::Keyboard { keycode: K_VK_TAB }),
        "esc" | "escape" => Some(KeyAction::Keyboard {
            keycode: K_VK_ESCAPE,
        }),
        "delete" | "backspace" => Some(KeyAction::Keyboard {
            keycode: K_VK_DELETE,
        }),
        "forward_delete" | "fwd_delete" | "del" => Some(KeyAction::Keyboard {
            keycode: K_VK_FORWARD_DELETE,
        }),
        "caps_lock" | "capslock" => Some(KeyAction::Keyboard {
            keycode: K_VK_CAPS_LOCK,
        }),

        "left_bracket" | "open_bracket" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_LEFT_BRACKET,
        }),
        "right_bracket" | "close_bracket" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_RIGHT_BRACKET,
        }),
        "semicolon" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_SEMICOLON,
        }),
        "quote" | "apostrophe" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_QUOTE,
        }),
        "comma" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_COMMA,
        }),
        "period" | "dot" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_PERIOD,
        }),
        "slash" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_SLASH,
        }),
        "minus" | "hyphen" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_MINUS,
        }),
        "equal" | "equals" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_EQUAL,
        }),

        "plus" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_EQUAL,
        }),
        "grave" | "backtick" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_GRAVE,
        }),
        "backslash" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_BACKSLASH,
        }),

        "f1" => Some(KeyAction::Keyboard { keycode: 0x7A }),
        "f2" => Some(KeyAction::Keyboard { keycode: 0x78 }),
        "f3" => Some(KeyAction::Keyboard { keycode: 0x63 }),
        "f4" => Some(KeyAction::Keyboard { keycode: 0x76 }),
        "f5" => Some(KeyAction::Keyboard { keycode: 0x60 }),
        "f6" => Some(KeyAction::Keyboard { keycode: 0x61 }),
        "f7" => Some(KeyAction::Keyboard { keycode: 0x62 }),
        "f8" => Some(KeyAction::Keyboard { keycode: 0x64 }),
        "f9" => Some(KeyAction::Keyboard { keycode: 0x65 }),
        "f10" => Some(KeyAction::Keyboard { keycode: 0x6D }),
        "f11" => Some(KeyAction::Keyboard { keycode: 0x67 }),
        "f12" => Some(KeyAction::Keyboard { keycode: 0x6F }),
        "f13" => Some(KeyAction::Keyboard { keycode: 0x69 }),
        "f14" => Some(KeyAction::Keyboard { keycode: 0x6B }),
        "f15" => Some(KeyAction::Keyboard { keycode: 0x71 }),
        "f16" => Some(KeyAction::Keyboard { keycode: 0x6A }),
        "f17" => Some(KeyAction::Keyboard { keycode: 0x40 }),
        "f18" => Some(KeyAction::Keyboard { keycode: 0x4F }),
        "f19" => Some(KeyAction::Keyboard { keycode: 0x50 }),
        "f20" => Some(KeyAction::Keyboard { keycode: 0x5A }),

        "volume_up" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_SOUND_UP,
        }),
        "volume_down" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_SOUND_DOWN,
        }),
        "mute" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_MUTE,
        }),
        "brightness_up" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_BRIGHTNESS_UP,
        }),
        "brightness_down" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_BRIGHTNESS_DOWN,
        }),
        "play_pause" | "playpause" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_PLAY,
        }),
        "next_track" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_NEXT,
        }),
        "prev_track" | "previous_track" => Some(KeyAction::Media {
            nx_key: K_NX_KEYTYPE_PREVIOUS,
        }),

        "mission_control" => Some(KeyAction::Keyboard {
            keycode: K_VK_MISSION_CONTROL,
        }),
        "launchpad" => Some(KeyAction::Keyboard {
            keycode: K_VK_LAUNCHPAD,
        }),
        _ => None,
    }
}

fn letter_keycode(c: char) -> Option<u16> {
    match c {
        'a' => Some(K_VK_ANSI_A),
        'b' => Some(K_VK_ANSI_B),
        'c' => Some(K_VK_ANSI_C),
        'd' => Some(K_VK_ANSI_D),
        'e' => Some(K_VK_ANSI_E),
        'f' => Some(K_VK_ANSI_F),
        'g' => Some(K_VK_ANSI_G),
        'h' => Some(K_VK_ANSI_H),
        'i' => Some(K_VK_ANSI_I),
        'j' => Some(K_VK_ANSI_J),
        'k' => Some(K_VK_ANSI_K),
        'l' => Some(K_VK_ANSI_L),
        'm' => Some(K_VK_ANSI_M),
        'n' => Some(K_VK_ANSI_N),
        'o' => Some(K_VK_ANSI_O),
        'p' => Some(K_VK_ANSI_P),
        'q' => Some(K_VK_ANSI_Q),
        'r' => Some(K_VK_ANSI_R),
        's' => Some(K_VK_ANSI_S),
        't' => Some(K_VK_ANSI_T),
        'u' => Some(K_VK_ANSI_U),
        'v' => Some(K_VK_ANSI_V),
        'w' => Some(K_VK_ANSI_W),
        'x' => Some(K_VK_ANSI_X),
        'y' => Some(K_VK_ANSI_Y),
        'z' => Some(K_VK_ANSI_Z),
        _ => None,
    }
}

fn digit_keycode(c: char) -> Option<u16> {
    match c {
        '0' => Some(K_VK_ANSI_0),
        '1' => Some(K_VK_ANSI_1),
        '2' => Some(K_VK_ANSI_2),
        '3' => Some(K_VK_ANSI_3),
        '4' => Some(K_VK_ANSI_4),
        '5' => Some(K_VK_ANSI_5),
        '6' => Some(K_VK_ANSI_6),
        '7' => Some(K_VK_ANSI_7),
        '8' => Some(K_VK_ANSI_8),
        '9' => Some(K_VK_ANSI_9),
        _ => None,
    }
}

fn punctuation_keycode(c: char) -> Option<u16> {
    match c {
        '[' => Some(K_VK_ANSI_LEFT_BRACKET),
        ']' => Some(K_VK_ANSI_RIGHT_BRACKET),
        ';' => Some(K_VK_ANSI_SEMICOLON),
        '\'' => Some(K_VK_ANSI_QUOTE),
        ',' => Some(K_VK_ANSI_COMMA),
        '.' => Some(K_VK_ANSI_PERIOD),
        '/' => Some(K_VK_ANSI_SLASH),
        '-' => Some(K_VK_ANSI_MINUS),
        '=' => Some(K_VK_ANSI_EQUAL),
        '`' => Some(K_VK_ANSI_GRAVE),
        '\\' => Some(K_VK_ANSI_BACKSLASH),
        _ => None,
    }
}

static HELD: std::sync::LazyLock<Mutex<HashMap<u32, PressedKey>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn handle_down(button: u32, key: &PressedKey) {
    let mut held = HELD.lock().unwrap();
    if held.contains_key(&button) {
        log_debug!(
            "[buttons] button {} already held, ignoring repeated down",
            button
        );
        return;
    }

    let held_elsewhere = held.values().fold(0, |acc, k| acc | k.modifiers);
    held.insert(button, *key);
    drop(held);
    enqueue_step(*key, true, held_elsewhere);
    log_debug!("[buttons] button {} down -> {:?}", button, key.action);
}

pub(crate) fn handle_up(button: u32) {
    let mut held = HELD.lock().unwrap();
    let Some(key) = held.remove(&button) else {
        log_debug!(
            "[buttons] button {} up without held state, ignoring",
            button
        );
        return;
    };

    let held_elsewhere = held.values().fold(0, |acc, k| acc | k.modifiers);
    drop(held);
    enqueue_step(key, false, held_elsewhere);
    log_debug!("[buttons] button {} up -> {:?}", button, key.action);
}

pub(crate) fn release_all() {
    let mut held = HELD.lock().unwrap();
    if held.is_empty() {
        return;
    }

    let mods_used = held.values().fold(0, |acc, k| acc | k.modifiers);
    let keys: Vec<PressedKey> = held.values().copied().collect();
    let n = held.len();
    held.clear();
    drop(held);
    enqueue_release_all(keys, mods_used);
    log_debug!("[buttons] released {} held button(s)", n);
}

#[cfg(not(test))]
enum KeyJob {
    Step {
        key: PressedKey,
        down: bool,
        held_elsewhere: CGEventFlags,
    },
    ReleaseAll {
        keys: Vec<PressedKey>,
        mods_used: CGEventFlags,
    },
    Flush(flume::Sender<()>),
}

#[cfg(not(test))]
static KEY_TX: std::sync::OnceLock<flume::Sender<KeyJob>> = std::sync::OnceLock::new();

#[cfg(not(test))]
fn key_sender() -> &'static flume::Sender<KeyJob> {
    KEY_TX.get_or_init(|| {
        let (tx, rx) = flume::unbounded::<KeyJob>();
        std::thread::Builder::new()
            .name("button-keys".into())
            .spawn(move || {
                let source = unsafe { CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_PRIVATE) };
                while let Ok(job) = rx.recv() {
                    objc2::rc::autoreleasepool(|_| unsafe {
                        match job {
                            KeyJob::Step {
                                key,
                                down,
                                held_elsewhere,
                            } => post_key_event(source, &key, down, held_elsewhere),
                            KeyJob::ReleaseAll { keys, mods_used } => {
                                let mut remaining = mods_used;
                                for key in &keys {
                                    if let KeyAction::Keyboard { keycode } = key.action
                                        && MODIFIER_KEYCODES.iter().any(|(_, vk)| *vk == keycode)
                                    {
                                        continue;
                                    }
                                    post_key_event(source, key, false, remaining);
                                }
                                for key in &keys {
                                    if let KeyAction::Keyboard { keycode } = key.action
                                        && let Some((flag, _)) =
                                            MODIFIER_KEYCODES.iter().find(|(_, vk)| *vk == keycode)
                                    {
                                        post_key_event(source, key, false, remaining);
                                        remaining &= !*flag;
                                    }
                                }
                                for (flag, vk) in MODIFIER_KEYCODES {
                                    if remaining & flag != 0 {
                                        post_modifier_key(source, vk, flag, false, remaining);
                                        remaining &= !flag;
                                    }
                                }
                            }
                            KeyJob::Flush(ack) => {
                                let _ = ack.send(());
                            }
                        }
                    });
                }
            })
            .expect("failed to spawn button keys worker thread");
        tx
    })
}

#[cfg(not(test))]
fn enqueue_step(key: PressedKey, down: bool, held_elsewhere: CGEventFlags) {
    let _ = key_sender().send(KeyJob::Step {
        key,
        down,
        held_elsewhere,
    });
}

#[cfg(not(test))]
fn enqueue_release_all(keys: Vec<PressedKey>, mods_used: CGEventFlags) {
    let _ = key_sender().send(KeyJob::ReleaseAll { keys, mods_used });
}

#[cfg(not(test))]
pub(crate) fn flush() {
    if let Some(tx) = KEY_TX.get() {
        let (ack_tx, ack_rx) = flume::bounded(1);
        if tx.send(KeyJob::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(std::time::Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
fn enqueue_step(_key: PressedKey, _down: bool, _held_elsewhere: CGEventFlags) {}

#[cfg(test)]
fn enqueue_release_all(_keys: Vec<PressedKey>, _mods_used: CGEventFlags) {}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn flush() {}

#[cfg(not(test))]
const MODIFIER_KEYCODES: [(CGEventFlags, u16); 4] = [
    (K_CG_EVENT_FLAG_MASK_COMMAND, K_VK_COMMAND),
    (K_CG_EVENT_FLAG_MASK_CONTROL, K_VK_CONTROL),
    (K_CG_EVENT_FLAG_MASK_ALTERNATE, K_VK_OPTION),
    (K_CG_EVENT_FLAG_MASK_SHIFT, K_VK_SHIFT),
];

#[cfg(not(test))]
unsafe fn post_key_event(
    source: CGEventSourceRef,
    key: &PressedKey,
    down: bool,
    held_elsewhere: CGEventFlags,
) {
    unsafe {
        match key.action {
            KeyAction::Keyboard { keycode } => {
                let is_modifier_main = MODIFIER_KEYCODES.iter().any(|(_, vk)| *vk == keycode);
                if down {
                    let mut held = held_elsewhere;
                    let mut pressed_any_modifier = false;
                    if !is_modifier_main {
                        for (flag, vk) in MODIFIER_KEYCODES {
                            if key.modifiers & flag != 0 && held & flag == 0 {
                                post_modifier_key(source, vk, flag, true, held);
                                held |= flag;
                                pressed_any_modifier = true;
                            }
                        }
                    }

                    if pressed_any_modifier {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    post_simple_key(
                        source,
                        keycode,
                        true,
                        effective_flags(key, true, held_elsewhere),
                    );
                } else {
                    post_simple_key(
                        source,
                        keycode,
                        false,
                        effective_flags(key, false, held_elsewhere),
                    );
                    if !is_modifier_main {
                        let mut held = held_elsewhere | key.modifiers;
                        for (flag, vk) in MODIFIER_KEYCODES {
                            if key.modifiers & flag != 0 && held_elsewhere & flag == 0 {
                                post_modifier_key(source, vk, flag, false, held);
                                held &= !flag;
                            }
                        }
                    }
                }
            }
            KeyAction::Media { nx_key } => {
                post_media_event(nx_key, down);
            }
        }
    }
}

#[cfg(not(test))]
unsafe fn post_simple_key(
    source: CGEventSourceRef,
    keycode: u16,
    down: bool,
    flags: CGEventFlags,
) {
    unsafe {
        let ev = CGEventCreateKeyboardEvent(source, keycode, if down { 1 } else { 0 });
        if ev.is_null() {
            log_debug!("[buttons] failed to synthesize keyboard event");
            return;
        }

        let merged = merge_event_flags(CGEventGetFlags(ev), flags);
        CGEventSetFlags(ev, merged);
        CGEventPost(K_CG_HID_EVENT_TAP, ev);
        crate::ffi::CFRelease(ev as *const std::ffi::c_void);
    }
}

const MODIFIER_FLAG_MASK: CGEventFlags = K_CG_EVENT_FLAG_MASK_COMMAND
    | K_CG_EVENT_FLAG_MASK_CONTROL
    | K_CG_EVENT_FLAG_MASK_ALTERNATE
    | K_CG_EVENT_FLAG_MASK_SHIFT;

const DEVICE_MODIFIER_FLAG_MASK: CGEventFlags = 0x0000_207F;
const NX_DEVICE_LCTL_KEY_MASK: CGEventFlags = 0x0000_0001;
const NX_DEVICE_LSHIFT_KEY_MASK: CGEventFlags = 0x0000_0002;
const NX_DEVICE_LCMD_KEY_MASK: CGEventFlags = 0x0000_0008;
const NX_DEVICE_LALT_KEY_MASK: CGEventFlags = 0x0000_0020;

fn device_modifier_flags(flags: CGEventFlags) -> CGEventFlags {
    let mut out: CGEventFlags = 0;
    if flags & K_CG_EVENT_FLAG_MASK_CONTROL != 0 {
        out |= NX_DEVICE_LCTL_KEY_MASK;
    }
    if flags & K_CG_EVENT_FLAG_MASK_SHIFT != 0 {
        out |= NX_DEVICE_LSHIFT_KEY_MASK;
    }
    if flags & K_CG_EVENT_FLAG_MASK_COMMAND != 0 {
        out |= NX_DEVICE_LCMD_KEY_MASK;
    }
    if flags & K_CG_EVENT_FLAG_MASK_ALTERNATE != 0 {
        out |= NX_DEVICE_LALT_KEY_MASK;
    }
    out
}

fn merge_event_flags(inherited: CGEventFlags, flags: CGEventFlags) -> CGEventFlags {
    let base = inherited & !(MODIFIER_FLAG_MASK | DEVICE_MODIFIER_FLAG_MASK);
    base | flags | device_modifier_flags(flags)
}

#[cfg(not(test))]
unsafe fn post_modifier_key(
    source: CGEventSourceRef,
    vk: u16,
    flag: CGEventFlags,
    down: bool,
    held: CGEventFlags,
) {
    let flags = if down { held | flag } else { held & !flag };
    unsafe { post_simple_key(source, vk, down, flags) };
}

fn effective_flags(key: &PressedKey, down: bool, held_elsewhere: CGEventFlags) -> CGEventFlags {
    let mut flags = held_elsewhere | key.modifiers;
    if let KeyAction::Keyboard { keycode } = key.action {
        let own = match keycode {
            K_VK_COMMAND => K_CG_EVENT_FLAG_MASK_COMMAND,
            K_VK_CONTROL => K_CG_EVENT_FLAG_MASK_CONTROL,
            K_VK_OPTION => K_CG_EVENT_FLAG_MASK_ALTERNATE,
            K_VK_SHIFT => K_CG_EVENT_FLAG_MASK_SHIFT,
            _ => 0,
        };
        if own != 0 {
            if down {
                flags |= own;
            } else {
                flags &= !own;
            }
        }
    }
    flags
}

#[cfg(not(test))]
unsafe fn post_media_event(nx_key: i32, down: bool) {
    let state: isize = if down { 0xa } else { 0xb };
    let data1: isize = ((nx_key as isize) << 16) | (state << 8);
    let modifier_flags: u64 = if down { 0xa00 } else { 0xb00 };
    unsafe {
        let event = ffi::objc_msgSend_create_system_event(
            class!(NSEvent) as *const objc2::runtime::AnyClass as *mut AnyObject,
            sel!(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:),
            14,
            ffi::NSPointFFI { x: 0.0, y: 0.0 },
            modifier_flags,
            0.0,
            0,
            std::ptr::null_mut(),
            8,
            data1,
            -1,
        );
        if event.is_null() {
            log_debug!("[buttons] failed to synthesize NSSystemDefined event");
            return;
        }
        ffi::objc_msgSend_retain(event, sel!(retain));
        let cg_event = ffi::objc_msgSend_cgevent(event, sel!(CGEvent));
        if !cg_event.is_null() {
            CGEventPost(K_CG_HID_EVENT_TAP, cg_event);
        }
        ffi::objc_msgSend_release(event, sel!(release));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mouse::ffi;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{class, msg_send, sel};

    #[test]
    fn parses_modifier_aliases_and_case() {
        for (raw, expected_flag) in [
            ("cmd+x", K_CG_EVENT_FLAG_MASK_COMMAND),
            ("command+x", K_CG_EVENT_FLAG_MASK_COMMAND),
            ("Ctrl+X", K_CG_EVENT_FLAG_MASK_CONTROL),
            ("control+x", K_CG_EVENT_FLAG_MASK_CONTROL),
            ("opt+x", K_CG_EVENT_FLAG_MASK_ALTERNATE),
            ("option+x", K_CG_EVENT_FLAG_MASK_ALTERNATE),
            ("alt+x", K_CG_EVENT_FLAG_MASK_ALTERNATE),
            ("shift+x", K_CG_EVENT_FLAG_MASK_SHIFT),
        ] {
            let k = parse_action(raw).expect(raw);
            assert_eq!(k.modifiers, expected_flag, "{raw}");
            assert_eq!(
                k.action,
                KeyAction::Keyboard {
                    keycode: K_VK_ANSI_X
                }
            );
        }
    }

    #[test]
    fn parses_combined_modifiers() {
        let k = parse_action("cmd+ctrl+opt+shift+space").unwrap();
        assert_eq!(
            k.modifiers,
            K_CG_EVENT_FLAG_MASK_COMMAND
                | K_CG_EVENT_FLAG_MASK_CONTROL
                | K_CG_EVENT_FLAG_MASK_ALTERNATE
                | K_CG_EVENT_FLAG_MASK_SHIFT
        );
        assert_eq!(
            k.action,
            KeyAction::Keyboard {
                keycode: K_VK_SPACE
            }
        );
    }

    #[test]
    fn parses_common_key_names() {
        let cases: &[(&str, u16)] = &[
            ("a", K_VK_ANSI_A),
            ("z", K_VK_ANSI_Z),
            ("0", K_VK_ANSI_0),
            ("9", K_VK_ANSI_9),
            ("[", K_VK_ANSI_LEFT_BRACKET),
            ("]", K_VK_ANSI_RIGHT_BRACKET),
            (";", K_VK_ANSI_SEMICOLON),
            ("'", K_VK_ANSI_QUOTE),
            (",", K_VK_ANSI_COMMA),
            (".", K_VK_ANSI_PERIOD),
            ("/", K_VK_ANSI_SLASH),
            ("-", K_VK_ANSI_MINUS),
            ("=", K_VK_ANSI_EQUAL),
            ("`", K_VK_ANSI_GRAVE),
            ("\\", K_VK_ANSI_BACKSLASH),
            ("left", K_VK_LEFT_ARROW),
            ("right", K_VK_RIGHT_ARROW),
            ("up", K_VK_UP_ARROW),
            ("down", K_VK_DOWN_ARROW),
            ("arrow_left", K_VK_LEFT_ARROW),
            ("home", K_VK_HOME),
            ("end", K_VK_END),
            ("page_up", K_VK_PAGE_UP),
            ("page_down", K_VK_PAGE_DOWN),
            ("space", K_VK_SPACE),
            ("return", K_VK_RETURN),
            ("enter", K_VK_RETURN),
            ("tab", K_VK_TAB),
            ("esc", K_VK_ESCAPE),
            ("escape", K_VK_ESCAPE),
            ("delete", K_VK_DELETE),
            ("forward_delete", K_VK_FORWARD_DELETE),
            ("caps_lock", K_VK_CAPS_LOCK),
            ("f1", 0x7A),
            ("f20", 0x5A),
            ("plus", K_VK_ANSI_EQUAL),
            ("mission_control", K_VK_MISSION_CONTROL),
            ("launchpad", K_VK_LAUNCHPAD),
        ];
        for (raw, kc) in cases {
            let k = parse_action(raw).unwrap_or_else(|| panic!("{raw} should parse"));
            assert_eq!(k.action, KeyAction::Keyboard { keycode: *kc }, "{raw}");
        }
    }

    #[test]
    fn parses_media_keys() {
        let cases: &[(&str, i32)] = &[
            ("volume_up", K_NX_KEYTYPE_SOUND_UP),
            ("volume_down", K_NX_KEYTYPE_SOUND_DOWN),
            ("mute", K_NX_KEYTYPE_MUTE),
            ("brightness_up", K_NX_KEYTYPE_BRIGHTNESS_UP),
            ("brightness_down", K_NX_KEYTYPE_BRIGHTNESS_DOWN),
            ("play_pause", K_NX_KEYTYPE_PLAY),
            ("next_track", K_NX_KEYTYPE_NEXT),
            ("prev_track", K_NX_KEYTYPE_PREVIOUS),
        ];
        for (raw, nx) in cases {
            let k = parse_action(raw).unwrap_or_else(|| panic!("{raw} should parse"));
            assert_eq!(k.action, KeyAction::Media { nx_key: *nx }, "{raw}");
        }
    }

    #[test]
    fn parses_lone_modifier_as_main_key() {
        let k = parse_action("cmd").unwrap();
        assert_eq!(k.modifiers, 0);
        assert_eq!(
            k.action,
            KeyAction::Keyboard {
                keycode: K_VK_COMMAND
            }
        );

        let k = parse_action("cmd+shift").unwrap();
        assert_eq!(k.modifiers, K_CG_EVENT_FLAG_MASK_COMMAND);
        assert_eq!(
            k.action,
            KeyAction::Keyboard {
                keycode: K_VK_SHIFT
            }
        );
    }

    #[test]
    fn rejects_invalid_actions() {
        for raw in [
            "",
            "   ",
            "+x",
            "x+",
            "cmd++x",
            "cmd+cmd+x",
            "shift+shift",
            "cmd+foo",
            "foo+bar",
        ] {
            assert!(parse_action(raw).is_none(), "{raw} should be rejected");
        }

        assert!(parse_action("ctrl+cmd+x").is_some());
    }

    #[test]
    fn down_up_pairs() {
        let key = parse_action("cmd+left").unwrap();
        handle_down(3, &key);
        {
            let held = HELD.lock().unwrap();
            assert_eq!(held.get(&3), Some(&key));
        }
        handle_up(3);
        {
            let held = HELD.lock().unwrap();
            assert!(!held.contains_key(&3));
        }
    }

    #[test]
    fn repeated_down_is_idempotent() {
        let key = parse_action("ctrl+right").unwrap();
        handle_down(4, &key);
        handle_down(4, &key);
        let held = HELD.lock().unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held.get(&4), Some(&key));
    }

    #[test]
    fn unexpected_up_is_safe() {
        handle_up(3);
        handle_up(4);
        assert!(HELD.lock().unwrap().is_empty());
    }

    #[test]
    fn release_all_clears_held() {
        let k1 = parse_action("cmd").unwrap();
        let k2 = parse_action("opt+space").unwrap();
        handle_down(3, &k1);
        handle_down(4, &k2);
        release_all();
        assert!(HELD.lock().unwrap().is_empty());

        handle_up(3);
        handle_up(4);
        assert!(HELD.lock().unwrap().is_empty());
    }

    #[test]
    fn effective_flags_handles_down_up_and_held_elsewhere() {
        let k = parse_action("cmd").unwrap();
        assert_eq!(effective_flags(&k, true, 0), K_CG_EVENT_FLAG_MASK_COMMAND);
        assert_eq!(effective_flags(&k, false, 0), 0);
        assert_eq!(
            effective_flags(&k, false, K_CG_EVENT_FLAG_MASK_SHIFT),
            K_CG_EVENT_FLAG_MASK_SHIFT
        );

        let k = parse_action("cmd+shift").unwrap();
        assert_eq!(
            effective_flags(&k, true, 0),
            K_CG_EVENT_FLAG_MASK_COMMAND | K_CG_EVENT_FLAG_MASK_SHIFT
        );
        assert_eq!(effective_flags(&k, false, 0), K_CG_EVENT_FLAG_MASK_COMMAND);

        let k = parse_action("ctrl+right").unwrap();
        assert_eq!(effective_flags(&k, true, 0), K_CG_EVENT_FLAG_MASK_CONTROL);
        assert_eq!(
            effective_flags(&k, false, K_CG_EVENT_FLAG_MASK_SHIFT),
            K_CG_EVENT_FLAG_MASK_CONTROL | K_CG_EVENT_FLAG_MASK_SHIFT
        );
    }

    #[test]
    fn merge_event_flags_replaces_device_modifier_bits_and_keeps_arrow_bits() {
        let inherited_arrow = 0x20a0_0000 | K_CG_EVENT_FLAG_MASK_COMMAND | NX_DEVICE_LCMD_KEY_MASK;
        assert_eq!(
            merge_event_flags(inherited_arrow, K_CG_EVENT_FLAG_MASK_CONTROL),
            0x20a0_0000 | K_CG_EVENT_FLAG_MASK_CONTROL | NX_DEVICE_LCTL_KEY_MASK
        );

        let inherited_ctrl = 0x2004_0001;
        assert_eq!(merge_event_flags(inherited_ctrl, 0), 0x2000_0000);
    }

    #[test]
    fn system_defined_event_ffi_roundtrip() {

        unsafe {
            let event = ffi::objc_msgSend_create_system_event(
                class!(NSEvent) as *const AnyClass as *mut AnyObject,
                sel!(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:),
                14,
                ffi::NSPointFFI { x: 0.0, y: 0.0 },
                0xa00,
                0.0,
                0,
                std::ptr::null_mut(),
                8,
                (16isize << 16) | (0xa << 8),
                -1,
            );
            assert!(!event.is_null(), "NSEvent creation failed");
            ffi::objc_msgSend_retain(event, sel!(retain));
            let cg = ffi::objc_msgSend_cgevent(event, sel!(CGEvent));
            assert!(!cg.is_null(), "NSEvent -> CGEvent failed");
            let data1: isize = msg_send![event, data1];
            assert_eq!(data1, (16 << 16) | (0xa << 8));
            ffi::objc_msgSend_release(event, sel!(release));
        }
    }
}
