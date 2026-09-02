//! 滚轮滚动模式:默认(透传+可反转)/按行(固定行数)/连续(触控板式像素滚动)。
//!
//! 方向语义:HID 层 event tap 看到的事件已包含系统"自然滚动"的翻转(自然滚动在 HID 事件
//! 生成时已应用),合成事件 post 到 session 层后不会再被系统翻转。因此方向处理只需按
//! 用户反转开关取反即可(见 should_flip),与 LinearMouse 一致——不读自然滚动设置。
//!
//! Scroll modes: Default (passthrough + optional reverse) / Line (fixed line count) /
//! Continuous (trackpad-style pixel scrolling).
//!
//! Direction semantics: events seen by the HID-level tap already include the system's natural-scroll
//! flip (applied when HID events are generated), and synthetic events posted to the session layer
//! are not flipped again by the system. Direction handling therefore only needs the user's reverse
//! toggle (see should_flip), matching LinearMouse -- no natural-scroll setting is read.

// ========== 方向处理 / direction handling ==========

/// 是否应对滚动 delta 取反:直接取用户反转开关。
/// HID tap 看到的事件已含系统自然滚动翻转,合成事件不再被翻转,所以:
/// - 反转关 -> 不取反(透传系统方向,含自然滚动)
/// - 反转开 -> 取反(相对系统的反转)
///
/// Whether to flip the scroll delta: directly the user's reverse toggle.
/// HID-tap events already carry the system natural-scroll flip and synthetic events aren't flipped
/// again, so:
/// - reverse off -> no flip (passthrough, including natural scrolling)
/// - reverse on  -> flip (reversed relative to the system)
pub(crate) fn should_flip(user_reverse: bool) -> bool {
    user_reverse
}

// ========== 滚动模式 / Scroll mode ==========

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ScrollMode {
    Default,
    Line,
    Continuous,
}

impl ScrollMode {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "line" => Self::Line,
            "continuous" => Self::Continuous,
            _ => Self::Default,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Line => "line",
            Self::Continuous => "continuous",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn current() -> Self {
        // 兼容旧路径(无设备上下文):用"所有鼠标 + 当前前台 app"解析。
        // Legacy path (no device context): resolve with the current frontmost app.
        let r = crate::mouse::resolve::resolve(None, crate::app::active_identity());
        r.scroll_mode
    }

    #[allow(dead_code)]
    pub(crate) fn all_labels() -> &'static [&'static str] {
        &["default", "line", "continuous"]
    }
}

/// 根据解析后的配置计算要 post 的滚动 delta。
/// 处理反转 + 行模式的行数归一化。结果形参供 post_scroll_event 使用。
///
/// Compute the scroll delta to post from the resolved config.
/// Handles reversal + line-mode normalization.
pub(crate) fn compute_delta(
    dy: i64,
    dx: i64,
    r: &crate::mouse::resolve::ResolvedMouse,
) -> (i32, i32) {
    let mode = r.scroll_mode;
    let flip = should_flip(r.reverse_scroll);

    let (mut ndy, mut ndx) = match mode {
        ScrollMode::Default => (dy as i32, dx as i32),
        ScrollMode::Line => {
            let line_count = r.line_count.clamp(1, crate::config::MAX_LINE_COUNT) as i64;
            let sign_y = if dy != 0 { dy.signum() } else { 0 };
            let sign_x = if dx != 0 { dx.signum() } else { 0 };
            ((sign_y * line_count) as i32, (sign_x * line_count) as i32)
        }
        // 连续模式由 event_tap 单独读取 point delta 后合成,这里保留透传兜底。
        // Continuous mode reads point deltas separately in event_tap; keep passthrough here.
        ScrollMode::Continuous => (dy as i32, dx as i32),
    };

    if flip {
        ndy = -ndy;
        ndx = -ndx;
    }

    (ndy, ndx)
}

/// 连续滚动模式下的像素 delta 计算:只应用用户反转开关,不套用 line_count。
/// Continuous-mode pixel delta: apply only the reverse toggle, never line_count.
pub(crate) fn compute_continuous_delta(
    point_dy: i64,
    point_dx: i64,
    r: &crate::mouse::resolve::ResolvedMouse,
) -> (i64, i64) {
    let flip = should_flip(r.reverse_scroll);
    let (mut ndy, mut ndx) = (point_dy, point_dx);
    if flip {
        ndy = -ndy;
        ndx = -ndx;
    }
    (ndy, ndx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_equals_user_reverse() {
        // 反转关 -> 不取反(透传系统方向,含自然滚动)。
        assert!(!should_flip(false));
        // 反转开 -> 取反(相对系统的反转)。
        assert!(should_flip(true));
    }

    #[test]
    fn scroll_mode_parses_all_labels() {
        assert_eq!(ScrollMode::from_str("default"), ScrollMode::Default);
        assert_eq!(ScrollMode::from_str("line"), ScrollMode::Line);
        assert_eq!(ScrollMode::from_str("continuous"), ScrollMode::Continuous);
        assert_eq!(ScrollMode::from_str("unknown"), ScrollMode::Default);
        assert_eq!(ScrollMode::all_labels(), &["default", "line", "continuous"]);
    }

    #[test]
    fn continuous_delta_does_not_use_line_count() {
        let mut r = crate::mouse::resolve::ResolvedMouse::default();
        r.scroll_mode = ScrollMode::Continuous;
        r.line_count = 15;
        r.reverse_scroll = false;
        assert_eq!(compute_continuous_delta(3, 4, &r), (3, 4));

        r.reverse_scroll = true;
        assert_eq!(compute_continuous_delta(3, 4, &r), (-3, -4));
    }
}
