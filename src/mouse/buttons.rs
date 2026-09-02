//! 侧键按钮映射(button_map):把侧键动作字符串解析成键盘/媒体键动作,
//! 按住侧键 = 按下目标键,松开 = 释放目标键,不自动重复。
//!
//! 交互语义对齐 LinearMouse / MOS 的按钮绑定体验:选中侧键、绑一个键盘动作,
//! 按住期间目标键保持按下,松开才释放。本仓库没有设置窗口,动作串在 TOML 里手写,
//! 通过 Reload Config 生效。
//!
//! Side-button mapping (button_map): parses an action string into a keyboard/media-key
//! action. Holding the side button presses the target key; releasing it releases the
//! target; no auto-repeat.
//!
//! Interaction semantics follow LinearMouse / MOS button binding: pick a button, bind one
//! keyboard action, hold to keep it pressed and release on button-up. This repo has no
//! settings window, so action strings are written in TOML and applied via Reload Config.

use crate::log_debug;
#[cfg(not(test))]
use crate::mouse::ffi::{
    self, CGEventCreateKeyboardEvent, CGEventGetFlags, CGEventPost, CGEventSetFlags,
    K_CG_HID_EVENT_TAP,
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

/// 已解析的按键动作:修饰键 flags + 主键。
/// A parsed key action: modifier flags + the main key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PressedKey {
    /// CGEventFlags 修饰键位(cmd/ctrl/opt/shift)。
    /// CGEventFlags modifier bits (cmd/ctrl/opt/shift).
    pub modifiers: CGEventFlags,
    pub action: KeyAction,
}

/// 主键类型。
/// The main key type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyAction {
    /// 普通键盘键(字母/数字/标点/方向键/功能键,以及 best-effort 的
    /// Mission Control / Launchpad)。
    /// Regular keyboard key (letters/digits/punctuation/arrows/F-keys, plus best-effort
    /// Mission Control / Launchpad).
    Keyboard { keycode: u16 },
    /// 媒体/系统功能键(NSSystemDefined / NX_KEYTYPE)。
    /// Media/system function keys (NSSystemDefined / NX_KEYTYPE).
    Media { nx_key: i32 },
}

// ========== 动作串解析 / action string parsing ==========

/// 解析动作串 `[modifier+]key`(修饰键可带 command/control/option/alt 别名)。
/// 大小写不敏感;单独一个修饰键也可作为主键(把侧键当 modifier 按住)。
/// 非法串返回 None(调用方丢弃该条映射,不影响其它配置)。
///
/// Parse an action string `[modifier+]key` (modifier aliases command/control/option/alt
/// accepted). Case-insensitive; a lone modifier is also valid as the main key (hold the
/// side button as a modifier). Invalid strings return None (callers drop that mapping
/// without touching other settings).
pub(crate) fn parse_action(raw: &str) -> Option<PressedKey> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    // 空段(如 "cmd++x" / "x+" / "+x")非法。
    // Empty segments (e.g. "cmd++x" / "x+" / "+x") are invalid.
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }

    let mut modifiers: CGEventFlags = 0;
    let mut seen = [false; 4];
    for p in &parts[..parts.len() - 1] {
        let (flag, idx) = modifier(p)?;
        // 重复修饰键(如 "cmd+cmd+x")非法。
        // Duplicate modifiers (e.g. "cmd+cmd+x") are invalid.
        if seen[idx] {
            return None;
        }
        seen[idx] = true;
        modifiers |= flag;
    }

    let last = parts[parts.len() - 1];
    // 末段是修饰键:当作主键按下(单独修饰键,或 "cmd+shift" 这类纯修饰键组合)。
    // Last token is a modifier: use it as the main key (lone modifier, or a pure-modifier
    // chord like "cmd+shift").
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

/// 修饰键 -> (flag, 去重用下标)。command/control/option/alt 是别名。
/// Modifier -> (flag, dedup index). command/control/option/alt are aliases.
fn modifier(name: &str) -> Option<(CGEventFlags, usize)> {
    match name {
        "cmd" | "command" => Some((K_CG_EVENT_FLAG_MASK_COMMAND, 0)),
        "ctrl" | "control" => Some((K_CG_EVENT_FLAG_MASK_CONTROL, 1)),
        "opt" | "option" | "alt" => Some((K_CG_EVENT_FLAG_MASK_ALTERNATE, 2)),
        "shift" => Some((K_CG_EVENT_FLAG_MASK_SHIFT, 3)),
        _ => None,
    }
}

/// 修饰键 -> 作为主键时的虚拟键码。
/// Modifier -> virtual keycode when used as the main key.
fn modifier_keycode(name: &str) -> Option<u16> {
    match name {
        "cmd" | "command" => Some(K_VK_COMMAND),
        "ctrl" | "control" => Some(K_VK_CONTROL),
        "opt" | "option" | "alt" => Some(K_VK_OPTION),
        "shift" => Some(K_VK_SHIFT),
        _ => None,
    }
}

/// 主键名 -> 动作(大小写已归一化)。覆盖计划要求的常用键。
/// Main key name -> action (name already lowercased). Covers the common keys from the plan.
fn key_action(name: &str) -> Option<KeyAction> {
    // 单字符:字母 / 数字 / 常用标点。
    // Single characters: letters / digits / common punctuation.
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
        // 方向键(同时接受 arrow_ 前缀的写法)。
        // Arrows (also accept arrow_ prefix).
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
        // 编辑键 / editing keys
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
        // 标点的词名别名 / word aliases for punctuation
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
        // 字面 '+' 键(US 布局与 '=' 同键码)。/ literal '+' key (same keycode as '=' on US).
        "plus" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_EQUAL,
        }),
        "grave" | "backtick" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_GRAVE,
        }),
        "backslash" => Some(KeyAction::Keyboard {
            keycode: K_VK_ANSI_BACKSLASH,
        }),
        // F1-F20 / F1 through F20
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
        // 媒体键 / media keys
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
        // 系统功能键(best-effort,见 ffi.rs 注释)。
        // System function keys (best-effort; see ffi.rs notes).
        "mission_control" => Some(KeyAction::Keyboard {
            keycode: K_VK_MISSION_CONTROL,
        }),
        "launchpad" => Some(KeyAction::Keyboard {
            keycode: K_VK_LAUNCHPAD,
        }),
        _ => None,
    }
}

/// 单字母 -> keycode(a-z)。
/// Single letter -> keycode (a-z).
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

/// 数字 -> keycode(0-9)。
/// Digit -> keycode (0-9).
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

/// 单字符标点 -> keycode(US 物理键位)。
/// Single-char punctuation -> keycode (US physical layout).
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

// ========== 按住状态 / held-button state ==========

/// 当前按住的侧键:按钮号(3/4)-> 目标键。所有操作都在鼠标线程 / 主线程互斥访问。
/// Currently held side buttons: button number (3/4) -> target key. Mutex-protected across
/// the mouse thread and the main thread.
static HELD: std::sync::LazyLock<Mutex<HashMap<u32, PressedKey>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// 侧键按下:发送目标键 down 并记录。重复 down(异常)忽略,避免重复按键。
/// Side button down: send the target key down and record it. Repeated down (abnormal) is
/// ignored to avoid double key presses.
pub(crate) fn handle_down(button: u32, key: &PressedKey) {
    let mut held = HELD.lock().unwrap();
    if held.contains_key(&button) {
        log_debug!(
            "[buttons] button {} already held, ignoring repeated down",
            button
        );
        return;
    }
    // 其他侧键已在按的修饰键不重复按下(防止 ctrl 双 down 卡住)。
    // Modifiers already held by other side buttons are not pressed again (avoids stuck
    // duplicate ctrl downs).
    let held_elsewhere = held.values().fold(0, |acc, k| acc | k.modifiers);
    unsafe {
        post_key_event(key, true, held_elsewhere);
    }
    held.insert(button, *key);
    log_debug!("[buttons] button {} down -> {:?}", button, key.action);
}

/// 侧键松开:发送目标键 up 并清除记录。未记录的 up(异常)忽略。
/// Side button up: send the target key up and clear the record. An up without a recorded
/// down (abnormal) is ignored.
pub(crate) fn handle_up(button: u32) {
    let mut held = HELD.lock().unwrap();
    let Some(key) = held.remove(&button) else {
        log_debug!(
            "[buttons] button {} up without held state, ignoring",
            button
        );
        return;
    };
    // 若还有其他侧键在用同一修饰键,不释放它。
    // Don't release a modifier still needed by other held side buttons.
    let held_elsewhere = held.values().fold(0, |acc, k| acc | k.modifiers);
    unsafe {
        post_key_event(&key, false, held_elsewhere);
    }
    log_debug!("[buttons] button {} up -> {:?}", button, key.action);
}

/// 释放所有按住的侧键目标键(配置重载 / 停用 / 设备断连重连时调用,
/// 防止映射到 cmd/ctrl/opt/shift 时修饰键卡住)。
///
/// Release every held side-button target (called on config reload / disable / device
/// plug-unplug, so modifiers like cmd/ctrl/opt/shift never get stuck).
pub(crate) fn release_all() {
    let mut held = HELD.lock().unwrap();
    if held.is_empty() {
        return;
    }
    // 循环内先只松开主键;所有用到的修饰键最后统一释放一次,避免重复 keyup。
    // Release only the main keys in the loop; release each used modifier once at the end
    // to avoid duplicate keyups.
    let mods_used = held.values().fold(0, |acc, k| acc | k.modifiers);
    for key in held.values() {
        unsafe {
            post_key_event(key, false, mods_used);
        }
    }
    let mut remaining = mods_used;
    for (flag, vk) in MODIFIER_KEYCODES {
        if mods_used & flag != 0 {
            unsafe {
                post_modifier_key(vk, flag, false, remaining);
            }
            remaining &= !flag;
        }
    }
    let n = held.len();
    held.clear();
    log_debug!("[buttons] released {} held button(s)", n);
}

// ========== 事件合成 / event synthesis ==========

/// 修饰键 flag -> 虚拟键码(用于先按下/后释放真实修饰键)。
/// Modifier flag -> virtual keycode (used to press/release real modifier keys).
const MODIFIER_KEYCODES: [(CGEventFlags, u16); 4] = [
    (K_CG_EVENT_FLAG_MASK_COMMAND, K_VK_COMMAND),
    (K_CG_EVENT_FLAG_MASK_CONTROL, K_VK_CONTROL),
    (K_CG_EVENT_FLAG_MASK_ALTERNATE, K_VK_OPTION),
    (K_CG_EVENT_FLAG_MASK_SHIFT, K_VK_SHIFT),
];

/// 发送一次目标键 down/up。普通键盘键用 CGEventCreateKeyboardEvent + flags
/// (post 到 HID 层,与媒体键一致——系统级快捷键如切换桌面只认 HID 层事件,
/// session 层能到达普通 App 但触发不了 Spaces/Mission Control);
/// 媒体/系统键走 NSSystemDefined + NX_KEYTYPE(post 到 HID 层,参考权威实现)。
/// 系统级快捷键(如 Mission Control 切桌面)依赖真实的修饰键按下状态,只给主键事件
/// 打 flag 位不够,所以带修饰键的动作会先按下修饰键、再按主键,松开时反序释放。
///
/// Send one target key down/up. Regular keyboard keys use CGEventCreateKeyboardEvent +
/// flags (posted at HID level, same as media keys — system-level shortcuts like space
/// switching only fire from HID-level events; the session level reaches regular apps but
/// not Spaces/Mission Control); media/system keys use NSSystemDefined + NX_KEYTYPE
/// (posted at HID level, per the reference implementation).
/// System-level shortcuts (e.g. Mission Control space switching) rely on the real modifier
/// key-down state, so flag bits on the main key event alone are not enough: modifier keys
/// are pressed first, then the main key; releases happen in reverse order.
#[cfg(not(test))]
unsafe fn post_key_event(key: &PressedKey, down: bool, held_elsewhere: CGEventFlags) {
    match key.action {
        KeyAction::Keyboard { keycode } => {
            let is_modifier_main = MODIFIER_KEYCODES.iter().any(|(_, vk)| *vk == keycode);
            if down {
                let mut held = held_elsewhere;
                let mut pressed_any_modifier = false;
                if !is_modifier_main {
                    for (flag, vk) in MODIFIER_KEYCODES {
                        if key.modifiers & flag != 0 && held & flag == 0 {
                            post_modifier_key(vk, flag, true, held);
                            held |= flag;
                            pressed_any_modifier = true;
                        }
                    }
                }
                // 系统快捷键依赖修饰键的真实按下状态:先让系统登记修饰键,再按主键。
                // 与主键同一瞬间发出时,切桌面等系统级快捷键可能不识别。
                // Give the system time to register the modifier state before the main key;
                // posted in the same instant, system-level shortcuts may not fire.
                if pressed_any_modifier {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                post_simple_key(keycode, true, effective_flags(key));
            } else {
                post_simple_key(keycode, false, effective_flags(key));
                if !is_modifier_main {
                    let mut held = held_elsewhere | key.modifiers;
                    for (flag, vk) in MODIFIER_KEYCODES {
                        if key.modifiers & flag != 0 && held_elsewhere & flag == 0 {
                            post_modifier_key(vk, flag, false, held);
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

#[cfg(not(test))]
unsafe fn post_simple_key(keycode: u16, down: bool, flags: CGEventFlags) {
    let ev = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, if down { 1 } else { 0 });
    if ev.is_null() {
        log_debug!("[buttons] failed to synthesize keyboard event");
        return;
    }
    // 关键修复:整体替换 flags 会抹掉 CGEventCreateKeyboardEvent 自动盖在
    // 方向键上的 SecondaryFn(0x800000)/NumericPad(0x200000) 位,导致
    // Ctrl+←/→ 这类系统级快捷键(切桌面)不识别——普通 App 只看 keycode,
    // 所以键照样收到,但系统快捷键匹配不到完整标志。
    // 只替换四个通用修饰位,保留 API 给出的继承位(LinearMouse 同款做法)。
    //
    // Key fix: replacing the whole flag word wipes the SecondaryFn/NumericPad
    // bits CGEventCreateKeyboardEvent stamps on arrow keys, so system-level
    // shortcuts (Ctrl+arrow space switching) stop matching — regular apps
    // still receive the key since they only look at keycode + generic flags.
    // Replace only the generic modifier bits and keep the inherited bits
    // (same approach as LinearMouse's KeySimulator).
    let inherited = CGEventGetFlags(ev) & !MODIFIER_FLAG_MASK;
    CGEventSetFlags(ev, inherited | flags);
    CGEventPost(K_CG_HID_EVENT_TAP, ev);
}

/// 四个通用修饰位:合成事件里只由动作串决定,不从继承 flags 带入。
/// The four generic modifier bits: determined only by the action string,
/// never inherited from the event-source state.
#[cfg_attr(test, allow(dead_code))]
const MODIFIER_FLAG_MASK: CGEventFlags = K_CG_EVENT_FLAG_MASK_COMMAND
    | K_CG_EVENT_FLAG_MASK_CONTROL
    | K_CG_EVENT_FLAG_MASK_ALTERNATE
    | K_CG_EVENT_FLAG_MASK_SHIFT;

/// 按下/释放一个真实修饰键。down 时 flags = 已按修饰键 + 自身;up 时 = 剩余修饰键。
/// Press/release one real modifier key. Down flags = held modifiers + itself; up flags =
/// the remaining modifiers.
#[cfg(not(test))]
unsafe fn post_modifier_key(vk: u16, flag: CGEventFlags, down: bool, held: CGEventFlags) {
    let flags = if down { held | flag } else { held & !flag };
    post_simple_key(vk, down, flags);
}

/// 测试环境:不真的往系统投递键盘事件,状态机测试只验证 held 记录。
/// Test builds: never post real events; state-machine tests only verify the held map.
#[cfg(test)]
unsafe fn post_key_event(_key: &PressedKey, _down: bool, _held_elsewhere: CGEventFlags) {}

#[cfg(test)]
unsafe fn post_modifier_key(_vk: u16, _flag: CGEventFlags, _down: bool, _held: CGEventFlags) {}

/// 事件 flags:组合修饰键 + 主键本身是修饰键时补上自身 flag(与真实按键事件一致)。
/// Event flags: combined modifiers + the main key's own flag when it is a modifier
/// (mirrors real key events).
fn effective_flags(key: &PressedKey) -> CGEventFlags {
    let mut flags = key.modifiers;
    if let KeyAction::Keyboard { keycode } = key.action {
        let own = match keycode {
            K_VK_COMMAND => K_CG_EVENT_FLAG_MASK_COMMAND,
            K_VK_CONTROL => K_CG_EVENT_FLAG_MASK_CONTROL,
            K_VK_OPTION => K_CG_EVENT_FLAG_MASK_ALTERNATE,
            K_VK_SHIFT => K_CG_EVENT_FLAG_MASK_SHIFT,
            _ => 0,
        };
        flags |= own;
    }
    flags
}

/// 媒体键:NSSystemDefined 事件。data1 高 16 位 = NX 键类型,低 8 位 = 按键状态
/// (0xa=down,0xb=up);modifierFlags 0xa00/0xb00 与状态位一致。
/// 事件对象 autorelease,这里显式 retain/release,不依赖线程上是否有 autorelease pool。
///
/// Media keys: NSSystemDefined events. data1's high 16 bits = NX key type, low 8 bits =
/// key state (0xa = down, 0xb = up); modifierFlags 0xa00/0xb00 mirror the state bits.
/// The event object is autoreleased; we retain/release explicitly so we don't depend on an
/// autorelease pool on this thread.
#[cfg(not(test))]
unsafe fn post_media_event(nx_key: i32, down: bool) {
    let state: isize = if down { 0xa } else { 0xb };
    let data1: isize = ((nx_key as isize) << 16) | (state << 8);
    let modifier_flags: u64 = if down { 0xa00 } else { 0xb00 };
    let event = ffi::objc_msgSend_create_system_event(
        class!(NSEvent) as *const objc2::runtime::AnyClass as *mut AnyObject,
        sel!(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:),
        14, // NSEventTypeSystemDefined
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mouse::ffi;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{class, msg_send, sel};

    // ========== parser ==========

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
        // 两个不同修饰键的组合合法。
        // A chord of two distinct modifiers is valid.
        assert!(parse_action("ctrl+cmd+x").is_some());
    }

    // ========== state machine ==========

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
        // 释放后 up 依然安全 / ups after release are safe.
        handle_up(3);
        handle_up(4);
        assert!(HELD.lock().unwrap().is_empty());
    }

    #[test]
    fn effective_flags_adds_own_modifier_flag() {
        let k = parse_action("cmd").unwrap();
        assert_eq!(effective_flags(&k), K_CG_EVENT_FLAG_MASK_COMMAND);
        let k = parse_action("cmd+shift").unwrap();
        assert_eq!(
            effective_flags(&k),
            K_CG_EVENT_FLAG_MASK_COMMAND | K_CG_EVENT_FLAG_MASK_SHIFT
        );
        let k = parse_action("cmd+left").unwrap();
        assert_eq!(effective_flags(&k), K_CG_EVENT_FLAG_MASK_COMMAND);
    }

    #[test]
    fn system_defined_event_ffi_roundtrip() {
        // 只构造事件并读回 data1,不调用 CGEventPost,无副作用。
        // Construct the event and read data1 back only; CGEventPost is never called, so
        // this has no side effects.
        unsafe {
            let event = ffi::objc_msgSend_create_system_event(
                class!(NSEvent) as *const AnyClass as *mut AnyObject,
                sel!(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:),
                14, // NSEventTypeSystemDefined
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
