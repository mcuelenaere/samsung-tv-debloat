//! The dock's geometry, motion and navigation, without EFL (host-tested).
//! `dock.rs` maps this state onto evas objects every animation frame.
//!
//! Three rows share one bar: the apps; Up opens the inputs, Down the
//! settings row (Samsung Home, Settings, TV suite). Down or Up goes back to
//! the apps; BACK always closes the dock. The selection glides between icons at constant speed; the
//! focused icon grows and rises, and a ring and a glow mark it.

// ---- Geometry, in pixels at 1920x1080 ----
pub const ICON: i32 = 92;
pub const FOCUS_ICON: i32 = 112;
pub const GAP: i32 = 18;
pub const PAD_X: i32 = 26;
pub const PAD_TOP: i32 = 16;
pub const LABEL_GAP: i32 = 8;
pub const LABEL_H: i32 = 34;
pub const PAD_BOTTOM: i32 = 14;
pub const BAR_H: i32 = PAD_TOP + FOCUS_ICON + LABEL_GAP + LABEL_H + PAD_BOTTOM;
pub const BAR_RADIUS: i32 = 24;
/// Room for the hint ("Inputs", "Settings") above and below the bar.
pub const HINT_H: i32 = 44;
/// Room for the bar's shadow.
pub const MARGIN: i32 = 36;
pub const MAX_BAR_W: i32 = 1808;
/// The selected icon rises this much.
pub const LIFT: i32 = 4;
pub const RING_W: i32 = 3;
pub const GLOW_SPREAD: i32 = 30;
/// Icons are drawn and clipped this far beyond the scrolling area, so the
/// grown icon and its ring are never cut at either end.
pub const CLIP_PAD: i32 = (FOCUS_ICON - ICON) / 2 + RING_W + 2;
const PITCH: i32 = ICON + GAP;

// ---- Motion, in seconds ----
pub const FOCUS_SECONDS: f64 = 0.3;
pub const SHOW_SECONDS: f64 = 0.3;
pub const HIDE_SECONDS: f64 = 0.16;
pub const IDLE_SECONDS: f64 = 20.0;
/// A row that replaces another starts this far shown, and slides this far.
const SWITCH_FROM: f32 = 0.4;
pub const SLIDE: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Apps,
    Inputs,
    Settings,
}

impl Row {
    pub const ALL: [Row; 3] = [Row::Apps, Row::Inputs, Row::Settings];

    pub fn index(self) -> usize {
        self as usize
    }

    /// Where Up goes, and its hint.
    pub fn up(self) -> Option<(Row, &'static str)> {
        match self {
            Row::Apps => Some((Row::Inputs, "Inputs")),
            Row::Settings => Some((Row::Apps, "Apps")),
            Row::Inputs => None,
        }
    }

    /// Where Down goes, and its hint.
    pub fn down(self) -> Option<(Row, &'static str)> {
        match self {
            Row::Apps => Some((Row::Settings, "Settings")),
            Row::Inputs => Some((Row::Apps, "Apps")),
            Row::Settings => None,
        }
    }
}

/// One row's bar and strip of icons, window-relative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strip {
    pub count: usize,
    pub bar: Rect,
    /// The scrolling area's width (the bar minus its padding).
    pub view_w: i32,
    strip_w: i32,
}

/// The dock window: as wide as the widest bar, at the bottom of the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub rect: Rect,
}

impl Window {
    pub fn new(screen_w: i32, screen_h: i32, counts: [usize; 3]) -> Self {
        let widest = counts
            .iter()
            .map(|count| bar_width(*count, screen_w))
            .max()
            .unwrap_or(0);
        let w = (widest + 2 * MARGIN).min(screen_w);
        let h = MARGIN + HINT_H + BAR_H + HINT_H + MARGIN / 2;
        Self {
            rect: Rect {
                x: (screen_w - w) / 2,
                y: screen_h - h,
                w,
                h,
            },
        }
    }

    pub fn strip(&self, count: usize, screen_w: i32) -> Strip {
        let bar_w = bar_width(count, screen_w);
        Strip {
            count,
            bar: Rect {
                x: (self.rect.w - bar_w) / 2,
                y: MARGIN + HINT_H,
                w: bar_w,
                h: BAR_H,
            },
            view_w: bar_w - 2 * PAD_X,
            strip_w: strip_width(count),
        }
    }
}

fn strip_width(count: usize) -> i32 {
    (count as i32 * PITCH - GAP).max(ICON)
}

fn bar_width(count: usize, screen_w: i32) -> i32 {
    (strip_width(count) + 2 * PAD_X)
        .min(MAX_BAR_W)
        .min(screen_w - 2 * MARGIN)
}

impl Strip {
    /// Scroll offset that keeps `focus` centred, clamped to the strip.
    pub fn scroll(&self, focus: f32) -> f32 {
        if self.strip_w <= self.view_w {
            return 0.0;
        }
        let wanted = ICON as f32 / 2.0 + focus * PITCH as f32 - self.view_w as f32 / 2.0;
        wanted.clamp(0.0, (self.strip_w - self.view_w) as f32)
    }

    /// Window-relative x of the centre of position `at` (fractional while the
    /// selection glides).
    pub fn center_x(&self, at: f32, scroll: f32) -> f32 {
        let start = if self.strip_w < self.view_w {
            (self.view_w - self.strip_w) as f32 / 2.0
        } else {
            0.0
        };
        (self.bar.x + PAD_X) as f32 + start + ICON as f32 / 2.0 + at * PITCH as f32 - scroll
    }

    /// Window-relative y of the icon row's centre.
    pub fn row_y(&self) -> i32 {
        self.bar.y + PAD_TOP + FOCUS_ICON / 2
    }

    /// The area icons are clipped to.
    pub fn clip(&self) -> Rect {
        Rect {
            x: self.bar.x + PAD_X - CLIP_PAD,
            y: 0,
            w: self.view_w + 2 * CLIP_PAD,
            h: i32::MAX / 2,
        }
    }

    /// Icon `index`'s box while the selection is at `focus`.
    pub fn icon(&self, index: usize, focus: f32, scroll: f32) -> Rect {
        let amount = (1.0 - (focus - index as f32).abs()).clamp(0.0, 1.0);
        // Even sizes only, so the centre never sits half a pixel off.
        let size = ICON + 2 * ((FOCUS_ICON - ICON) as f32 / 2.0 * amount).round() as i32;
        let x = self.center_x(index as f32, scroll).round() as i32 - size / 2;
        let y = self.row_y() - size / 2 - (LIFT as f32 * amount).round() as i32;
        Rect {
            x,
            y,
            w: size,
            h: size,
        }
    }

    /// A box of `side` centred on the selection, lifted like the focused icon.
    pub fn around_focus(&self, focus: f32, scroll: f32, side: i32) -> Rect {
        let x = self.center_x(focus, scroll).round() as i32 - side / 2;
        Rect {
            x,
            y: self.row_y() - side / 2 - LIFT,
            w: side,
            h: side,
        }
    }

    /// Left edge of a `width` label centred under `index`, kept inside the bar.
    pub fn label_x(&self, index: usize, scroll: f32, width: i32) -> i32 {
        let x = self.center_x(index as f32, scroll).round() as i32 - width / 2;
        let (low, high) = (self.bar.x + PAD_X, self.bar.x + self.bar.w - PAD_X - width);
        if high < low {
            self.bar.x + (self.bar.w - width) / 2
        } else {
            x.clamp(low, high)
        }
    }

    pub fn label_y(&self) -> i32 {
        self.bar.y + PAD_TOP + FOCUS_ICON + LABEL_GAP
    }
}

/// The selection of one row.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Focus {
    pub target: usize,
    pub position: f32,
    from: f32,
    elapsed: f64,
}

impl Focus {
    fn move_to(&mut self, index: usize) -> bool {
        if index == self.target {
            return false;
        }
        // A new run from wherever the selection is now.
        self.from = self.position;
        self.elapsed = 0.0;
        self.target = index;
        true
    }

    pub fn jump(&mut self, index: usize) {
        *self = Focus {
            target: index,
            position: index as f32,
            from: index as f32,
            elapsed: 0.0,
        };
    }

    fn tick(&mut self, dt: f64) -> bool {
        if self.position == self.target as f32 {
            return false;
        }
        self.elapsed += dt;
        let progress = (self.elapsed / FOCUS_SECONDS) as f32;
        self.position = if progress >= 1.0 {
            self.target as f32
        } else {
            self.from + (self.target as f32 - self.from) * progress
        };
        true
    }

    /// 0..1: how focused `index` is.
    pub fn amount(&self, index: usize) -> f32 {
        (1.0 - (self.position - index as f32).abs()).clamp(0.0, 1.0)
    }
}

/// How shown a row is, and where it slides from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fade {
    /// 0 gone .. 1 shown.
    pub presence: f32,
    /// +1 slides up into place, -1 down.
    pub direction: f32,
    showing: bool,
}

impl Fade {
    fn tick(&mut self, dt: f64) -> bool {
        let step = if self.showing {
            dt / SHOW_SECONDS
        } else {
            -dt / HIDE_SECONDS
        } as f32;
        let before = self.presence;
        self.presence = (self.presence + step).clamp(0.0, 1.0);
        self.presence != before
    }

    /// Vertical offset for the slide.
    pub fn offset(&self) -> i32 {
        ((1.0 - self.presence) * SLIDE * self.direction).round() as i32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Something moved: run frames.
    Moved,
    /// Open the focused entry of a row, then hide.
    Activate(Row, usize),
    /// The host picks the row's start (e.g. the current input) and calls
    /// [`Controller::open`].
    Open(Row),
    Hide,
}

#[derive(Debug, Default)]
pub struct Controller {
    pub row: Option<Row>,
    pub focus: [Focus; 3],
    pub fade: [Fade; 3],
    pub counts: [usize; 3],
}

impl Controller {
    pub fn new(counts: [usize; 3]) -> Self {
        Self {
            counts,
            ..Self::default()
        }
    }

    /// The row on screen, if the dock is shown (or showing).
    pub fn shown(&self) -> Option<Row> {
        self.row.filter(|row| self.fade[row.index()].showing)
    }

    /// New entries (a catalog refresh): focus stays in range.
    pub fn set_counts(&mut self, counts: [usize; 3]) {
        self.counts = counts;
        for row in Row::ALL {
            let last = counts[row.index()].saturating_sub(1);
            let focus = &mut self.focus[row.index()];
            if focus.target > last {
                focus.jump(last);
            }
        }
    }

    /// The dock appears on the apps row, on `focus`.
    pub fn show(&mut self, focus: usize) {
        for fade in &mut self.fade {
            *fade = Fade::default();
        }
        self.focus[Row::Apps.index()].jump(focus.min(self.counts[0].saturating_sub(1)));
        self.fade[0] = Fade {
            presence: 0.0,
            direction: 1.0,
            showing: true,
        };
        self.row = Some(Row::Apps);
    }

    pub fn hide(&mut self) {
        for fade in &mut self.fade {
            fade.showing = false;
        }
    }

    /// Replaces the shown row with `row`, focused on `focus`.
    pub fn open(&mut self, row: Row, focus: usize) {
        let Some(from) = self.row else { return };
        if from == row {
            return;
        }
        self.fade[from.index()].showing = false;
        // The new row comes from the side the arrow pointed to.
        let direction = if from.up().map(|(to, _)| to) == Some(row) {
            -1.0
        } else {
            1.0
        };
        self.fade[row.index()] = Fade {
            presence: SWITCH_FROM,
            direction,
            showing: true,
        };
        self.focus[row.index()].jump(focus.min(self.counts[row.index()].saturating_sub(1)));
        self.row = Some(row);
    }

    pub fn key(&mut self, key: &str) -> Action {
        let Some(row) = self.shown() else {
            return Action::None;
        };
        let focus = &mut self.focus[row.index()];
        let last = self.counts[row.index()].saturating_sub(1);
        let moved = |changed: bool| if changed { Action::Moved } else { Action::None };
        match key {
            "Left" => moved(focus.target > 0 && focus.move_to(focus.target - 1)),
            "Right" => moved(focus.target < last && focus.move_to(focus.target + 1)),
            "Up" => row.up().map_or(Action::None, |(to, _)| Action::Open(to)),
            "Down" => row.down().map_or(Action::None, |(to, _)| Action::Open(to)),
            "Return" | "KP_Enter" | "Select" if self.counts[row.index()] > 0 => {
                Action::Activate(row, focus.target)
            }
            // BACK always closes the dock, from any row.
            "XF86Back" | "Escape" | "BackSpace" | "XF86Exit" => Action::Hide,
            _ => Action::None,
        }
    }

    /// Advances the animations by `dt` seconds; whether another frame is
    /// needed.
    pub fn tick(&mut self, dt: f64) -> bool {
        let dt = dt.clamp(0.0, 0.1); // a stalled frame must not teleport anything
        let mut more = false;
        for index in 0..3 {
            more |= self.focus[index].tick(dt);
            more |= self.fade[index].tick(dt);
        }
        more
    }

    /// Fully gone (the window can be unmapped).
    pub fn gone(&self) -> bool {
        self.fade
            .iter()
            .all(|fade| fade.presence == 0.0 && !fade.showing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(controller: &mut Controller, seconds: f64) {
        let mut left = seconds;
        while left > 0.0 {
            controller.tick(1.0 / 60.0);
            left -= 1.0 / 60.0;
        }
    }

    #[test]
    fn the_window_fits_the_widest_row_at_the_bottom() {
        let window = Window::new(1920, 1080, [21, 4, 3]);
        assert_eq!(window.rect.w, MAX_BAR_W + 2 * MARGIN);
        assert_eq!(window.rect.y + window.rect.h, 1080);
        let small = window.strip(3, 1920);
        assert_eq!(small.bar.x, (window.rect.w - small.bar.w) / 2);
    }

    #[test]
    fn scrolling_keeps_the_focus_centred_and_clamped() {
        let window = Window::new(1920, 1080, [40, 4, 3]);
        let strip = window.strip(40, 1920);
        assert_eq!(strip.scroll(0.0), 0.0);
        let middle = strip.scroll(20.0);
        let centre = strip.center_x(20.0, middle);
        assert!((centre - (strip.bar.x + PAD_X + strip.view_w / 2) as f32).abs() < 1.0);
        assert_eq!(strip.scroll(39.0), strip.scroll(100.0));
        // A short row does not scroll and sits centred.
        let short = window.strip(3, 1920);
        assert_eq!(short.scroll(2.0), 0.0);
    }

    #[test]
    fn the_focused_icon_grows_and_rises() {
        let strip = Window::new(1920, 1080, [10, 4, 3]).strip(10, 1920);
        let focused = strip.icon(2, 2.0, 0.0);
        let other = strip.icon(3, 2.0, 0.0);
        assert_eq!((focused.w, other.w), (FOCUS_ICON, ICON));
        assert!(focused.y < other.y);
        let halfway = strip.icon(3, 2.5, 0.0);
        assert!(halfway.w > ICON && halfway.w < FOCUS_ICON && halfway.w % 2 == 0);
    }

    #[test]
    fn the_selection_glides_at_constant_speed() {
        let mut controller = Controller::new([10, 4, 3]);
        controller.show(0);
        assert_eq!(controller.key("Right"), Action::Moved);
        run(&mut controller, FOCUS_SECONDS / 2.0);
        assert!((controller.focus[0].position - 0.5).abs() < 0.06);
        run(&mut controller, FOCUS_SECONDS);
        assert_eq!(controller.focus[0].position, 1.0);
        // At rest no frames are asked for.
        assert!(!controller.tick(1.0 / 60.0));
        // The ends do not move.
        assert_eq!(controller.key("Left"), Action::Moved);
        run(&mut controller, 1.0);
        assert_eq!(controller.key("Left"), Action::None);
    }

    #[test]
    fn up_and_down_open_the_other_rows_and_come_back() {
        let mut controller = Controller::new([10, 4, 3]);
        controller.show(0);
        assert_eq!(controller.key("Up"), Action::Open(Row::Inputs));
        controller.open(Row::Inputs, 1);
        assert_eq!(controller.shown(), Some(Row::Inputs));
        assert_eq!(controller.focus[1].target, 1);
        assert_eq!(controller.fade[1].direction, -1.0);
        assert_eq!(controller.key("Up"), Action::None);
        assert_eq!(controller.key("XF86Back"), Action::Hide);
        assert_eq!(controller.key("Down"), Action::Open(Row::Apps));
        controller.open(Row::Apps, controller.focus[0].target);
        assert_eq!(controller.key("Down"), Action::Open(Row::Settings));
        controller.open(Row::Settings, 0);
        assert_eq!(controller.key("Return"), Action::Activate(Row::Settings, 0));
        assert_eq!(controller.key("Up"), Action::Open(Row::Apps));
    }

    #[test]
    fn back_on_the_apps_hides_and_the_fade_ends() {
        let mut controller = Controller::new([10, 4, 3]);
        controller.show(0);
        run(&mut controller, 1.0);
        assert_eq!(controller.fade[0].presence, 1.0);
        assert_eq!(controller.key("XF86Back"), Action::Hide);
        controller.hide();
        assert_eq!(controller.shown(), None);
        run(&mut controller, 1.0);
        assert!(controller.gone());
        assert_eq!(controller.key("Right"), Action::None);
    }

    #[test]
    fn a_shrinking_catalog_keeps_the_focus_in_range() {
        let mut controller = Controller::new([10, 4, 3]);
        controller.show(9);
        controller.set_counts([5, 4, 3]);
        assert_eq!(controller.focus[0].target, 4);
    }
}
