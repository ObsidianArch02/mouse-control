pub(crate) fn should_flip(user_reverse: bool) -> bool {
    user_reverse
}

pub(crate) const DEFAULT_LINE_HEIGHT: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollMode {
    Default,
    Line,
    Pixel,
    Discrete,
    Continuous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollUnit {
    Line,
    Pixel,
}

impl ScrollMode {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "line" => Self::Line,
            "pixel" => Self::Pixel,
            "discrete" => Self::Discrete,
            "continuous" => Self::Continuous,
            _ => Self::Default,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Line => "line",
            Self::Pixel => "pixel",
            Self::Discrete => "discrete",
            Self::Continuous => "continuous",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn current() -> Self {

        let r = crate::mouse::resolve::resolve(None, crate::app::active_identity());
        r.scroll_mode
    }

    #[allow(dead_code)]
    pub(crate) fn all_labels() -> &'static [&'static str] {
        &["default", "line", "pixel", "discrete", "continuous"]
    }
}

pub(crate) fn compute_delta(
    dy: i64,
    dx: i64,
    r: crate::mouse::resolve::ResolvedScroll,
) -> (i32, i32, ScrollUnit) {
    let mode = r.scroll_mode;
    let flip = should_flip(r.reverse_scroll);

    let (mut ndy, mut ndx, unit) = match mode {
        ScrollMode::Default => (dy as i32, dx as i32, ScrollUnit::Line),
        ScrollMode::Discrete => {
            let sign_y = dy.signum();
            let sign_x = dx.signum();
            let line_count = r.line_count.clamp(1, crate::config::MAX_LINE_COUNT) as i64;
            (
                (sign_y * line_count) as i32,
                (sign_x * line_count) as i32,
                ScrollUnit::Line,
            )
        }
        ScrollMode::Pixel => {
            let sign_y = dy.signum();
            let sign_x = dx.signum();
            let h = r
                .line_height
                .unwrap_or(DEFAULT_LINE_HEIGHT)
                .clamp(1, crate::config::MAX_LINE_HEIGHT) as i64;
            let magnitude = r.line_count.clamp(1, crate::config::MAX_LINE_COUNT) as i64 * h;
            (
                (sign_y * magnitude) as i32,
                (sign_x * magnitude) as i32,
                ScrollUnit::Pixel,
            )
        }
        ScrollMode::Line => {
            let sign_y = dy.signum();
            let sign_x = dx.signum();
            match r.line_height {

                Some(h) => {
                    let magnitude = (r.line_count.clamp(1, crate::config::MAX_LINE_COUNT) as i64
                        * h.clamp(1, crate::config::MAX_LINE_HEIGHT) as i64) as i32;
                    (
                        (sign_y * magnitude as i64) as i32,
                        (sign_x * magnitude as i64) as i32,
                        ScrollUnit::Pixel,
                    )
                }
                None => {
                    let line_count = r.line_count.clamp(1, crate::config::MAX_LINE_COUNT) as i64;
                    (
                        (sign_y * line_count) as i32,
                        (sign_x * line_count) as i32,
                        ScrollUnit::Line,
                    )
                }
            }
        }

        ScrollMode::Continuous => (dy as i32, dx as i32, ScrollUnit::Line),
    };

    if flip {
        ndy = -ndy;
        ndx = -ndx;
    }

    (ndy, ndx, unit)
}

pub(crate) fn compute_continuous_delta(
    point_dy: i64,
    point_dx: i64,
    r: crate::mouse::resolve::ResolvedScroll,
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

        assert!(!should_flip(false));

        assert!(should_flip(true));
    }

    #[test]
    fn scroll_mode_parses_all_labels() {
        assert_eq!(ScrollMode::from_str("default"), ScrollMode::Default);
        assert_eq!(ScrollMode::from_str("line"), ScrollMode::Line);
        assert_eq!(ScrollMode::from_str("pixel"), ScrollMode::Pixel);
        assert_eq!(ScrollMode::from_str("discrete"), ScrollMode::Discrete);
        assert_eq!(ScrollMode::from_str("continuous"), ScrollMode::Continuous);
        assert_eq!(ScrollMode::from_str("unknown"), ScrollMode::Default);
        assert_eq!(
            ScrollMode::all_labels(),
            &["default", "line", "pixel", "discrete", "continuous"]
        );
    }

    #[test]
    fn continuous_delta_does_not_use_line_count() {
        let mut r = crate::mouse::resolve::ResolvedScroll {
            scroll_mode: ScrollMode::Continuous,
            line_count: 15,
            reverse_scroll: false,
            ..Default::default()
        };
        assert_eq!(compute_continuous_delta(3, 4, r), (3, 4));

        r.reverse_scroll = true;
        assert_eq!(compute_continuous_delta(3, 4, r), (-3, -4));
    }

    #[test]
    fn line_mode_without_line_height_keeps_line_units() {
        let r = |reverse: bool| crate::mouse::resolve::ResolvedScroll {
            scroll_mode: ScrollMode::Line,
            line_count: 3,
            reverse_scroll: reverse,
            ..Default::default()
        };
        assert_eq!(compute_delta(1, 0, r(false)), (3, 0, ScrollUnit::Line));

        assert_eq!(compute_delta(1, 0, r(true)), (-3, 0, ScrollUnit::Line));
    }

    #[test]
    fn line_mode_with_line_height_uses_pixel_units() {
        let r = |reverse: bool| crate::mouse::resolve::ResolvedScroll {
            scroll_mode: ScrollMode::Line,
            line_count: 2,
            line_height: Some(24),
            reverse_scroll: reverse,
        };

        assert_eq!(compute_delta(1, 0, r(false)), (48, 0, ScrollUnit::Pixel));
        assert_eq!(compute_delta(-1, 0, r(false)), (-48, 0, ScrollUnit::Pixel));
        assert_eq!(compute_delta(0, 1, r(false)), (0, 48, ScrollUnit::Pixel));

        assert_eq!(compute_delta(1, 0, r(true)), (-48, 0, ScrollUnit::Pixel));
    }

    #[test]
    fn pixel_mode_uses_default_or_custom_line_height() {
        let default_h = crate::mouse::resolve::ResolvedScroll {
            scroll_mode: ScrollMode::Pixel,
            line_count: 1,
            line_height: None,
            reverse_scroll: false,
        };
        assert_eq!(
            compute_delta(5, 0, default_h),
            (DEFAULT_LINE_HEIGHT as i32, 0, ScrollUnit::Pixel)
        );

        let custom_h = crate::mouse::resolve::ResolvedScroll {
            scroll_mode: ScrollMode::Pixel,
            line_count: 2,
            line_height: Some(24),
            reverse_scroll: true,
        };
        assert_eq!(compute_delta(1, 0, custom_h), (-48, 0, ScrollUnit::Pixel));
    }

    #[test]
    fn discrete_mode_ignores_inherited_line_height() {
        let r = crate::mouse::resolve::ResolvedScroll {
            scroll_mode: ScrollMode::Discrete,
            line_count: 3,
            line_height: Some(24),
            reverse_scroll: true,
        };
        assert_eq!(compute_delta(7, 0, r), (-3, 0, ScrollUnit::Line));
        assert_eq!(compute_delta(-2, 0, r), (3, 0, ScrollUnit::Line));
    }
}
