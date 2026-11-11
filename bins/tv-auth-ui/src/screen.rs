//! The manager's screen (docs/tv-launcher-plan.md, the TV suite redesign):
//! a rail of tabs on the left and a page of blocks ([`page`]) on the right,
//! drawn with plain evas objects. A focus ring (or, on whole rows, a
//! highlight) glides between the focusable parts, the page scrolls smoothly
//! to keep focus in view, the rail's highlight slides between tabs, and a
//! new tab fades and rises in. An ecore animator runs only while something
//! moves; objects exist only for parts near the viewport.

use crate::kit::{self, premultiplied, sliced, Bounds, Obj};
use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::ptr;
use tv_auth_ui::page::{self, Block, Direction, Focusable, Layout, Measure, Part, Rect, Tone};
use tv_auth_ui::pixels;
use tv_efl::*;

pub const FONT: &str = "SamsungOneUI 500,Sans";
const FONT_REGULAR: &str = "SamsungOneUI 400,Sans";

const COLOR_PAGE: u32 = 0x11_15_1d;
const COLOR_RAIL: u32 = 0x0b_0e_14;
const COLOR_CARD: u32 = 0x1a_20_2b;
const COLOR_DIVIDER: u32 = 0x25_2c_38;
const COLOR_RAISED: u32 = 0x2b_34_46;
const COLOR_ACCENT: u32 = 0x4c_9a_ff;
const COLOR_DANGER: u32 = 0x5a_24_2d;
const COLOR_DANGER_TEXT: u32 = 0xff_b3_bd;
const COLOR_TEXT: u32 = 0xe9_ed_f3;
const COLOR_MUTED: u32 = 0x8a_93_a3;
const COLOR_CHIP: u32 = 0x25_2c_38;
const COLOR_CHIP_TEXT: u32 = 0xc8_cf_da;
const COLOR_CHIP_ON: u32 = 0x20_3a_60;
const COLOR_CHIP_ON_TEXT: u32 = 0xcf_e3_ff;
const COLOR_TRACK: u32 = 0x3a_42_52;
const COLOR_FIELD: u32 = 0x23_2a_37;
const COLOR_PILL: u32 = 0x23_2a_37;
const COLOR_PILL_ACTIVE: u32 = 0x3b_4a_66;

const RAIL_W: i32 = 320;
const RAIL_ITEM_Y: i32 = 168;
const RAIL_ITEM_H: i32 = 64;
const RAIL_ITEM_GAP: i32 = 10;
const VIEW_X: i32 = RAIL_W + 72;
const RIGHT_PAD: i32 = 80;
const TITLE_Y: i32 = 52;
const STATUS_Y: i32 = 122;
const VIEW_TOP: i32 = 190;
const BOTTOM_PAD: i32 = 32;
/// Parts are created and shown this far outside the viewport.
const CULL_MARGIN: i32 = 240;
/// Focus keeps this much room to the viewport's edges.
const SCROLL_MARGIN: i32 = 48;
const RING_GAP: i32 = 6;
const RING_W: usize = 3;
const PANEL_SLICE: i32 = 24;
const PILL_SLICE: i32 = 30;
/// Every motion closes this share of its remaining distance per second
/// (exponentially): quick, then settling without overshoot.
const SPEED: f64 = 18.0;
const TRANSITION_SECONDS: f64 = 0.28;
const TRANSITION_RISE: f32 = 28.0;

pub enum Event<A> {
    None,
    Action(A),
    /// The rail moved to another tab.
    Tab(usize),
    /// BACK; `in_rail` when focus was on the rail.
    Back {
        in_rail: bool,
    },
}

/// Pictures drawn once: white shapes tinted per use.
struct Art {
    panel: Vec<u32>,
    pill: Vec<u32>,
    ring: Vec<u32>,
    knob: Vec<u32>,
    /// Opaque outside rounded corners: laid over an icon in the colour
    /// behind it, it rounds the icon.
    corners: Vec<u32>,
}

impl Art {
    fn new() -> Self {
        let panel_side = (2 * PANEL_SLICE + 1) as usize;
        let pill_side = (2 * PILL_SLICE + 1) as usize;
        let rounded = pixels::panel(
            256,
            256.0 * pixels::RADIUS_SHARE,
            0xff_ff_ff,
            1.0,
            0.0,
            0,
            0.0,
        );
        let corners = rounded
            .iter()
            .map(|pixel| pixels::premultiply(0xff_ff_ff, 1.0 - (pixel >> 24) as f32 / 255.0))
            .collect();
        Self {
            panel: pixels::panel(panel_side, 20.0, 0xff_ff_ff, 1.0, 0.0, 0, 0.0),
            pill: pixels::panel(
                pill_side,
                PILL_SLICE as f32 - 2.0,
                0xff_ff_ff,
                1.0,
                0.0,
                0,
                0.0,
            ),
            ring: pixels::panel(
                panel_side,
                22.0,
                0xff_ff_ff,
                0.0,
                RING_W as f32,
                0xff_ff_ff,
                1.0,
            ),
            knob: pixels::dot(64, 0xff_ff_ff),
            corners,
        }
    }
}

/// The objects of one placed part, each at an offset from the part's
/// corner.
struct PartView {
    rect: Rect,
    part: Part,
    objects: Vec<(Obj, Bounds)>,
    created: bool,
}

/// A value moving toward a target.
#[derive(Clone, Copy, Default)]
struct Glide {
    now: f32,
    target: f32,
}

impl Glide {
    fn step(&mut self, factor: f32) -> bool {
        let gap = self.target - self.now;
        if gap.abs() < 0.5 {
            self.now = self.target;
            return false;
        }
        self.now += gap * factor;
        true
    }

    fn jump(&mut self, value: f32) {
        self.now = value;
        self.target = value;
    }
}

pub struct Screen<A> {
    window: *mut c_void,
    evas: *mut c_void,
    width: i32,
    height: i32,
    art: Art,
    background: Obj,
    rail_labels: Vec<Obj>,
    rail_pill: Obj,
    rail_ring: Obj,
    title: Obj,
    status: Obj,
    clip: Obj,
    ring: Obj,
    highlight: Obj,
    accent: Obj,
    parts: Vec<PartView>,
    layout: Layout<A>,
    tabs: Vec<(String, bool)>,
    tab: usize,
    in_rail: bool,
    focus: usize,
    scroll: Glide,
    ring_box: [Glide; 4],
    ring_alpha: Glide,
    highlight_alpha: Glide,
    pill_y: Glide,
    /// 0..1 while a new tab comes in.
    presence: f32,
    animator: *mut c_void,
    last_frame: f64,
    scratch_text: *mut c_void,
    scratch_block: *mut c_void,
    style: *mut c_void,
    field: *mut c_void,
    on_frame: EcoreTimerCb,
}

impl<A: Clone + PartialEq> Screen<A> {
    /// The chrome on `window` (`width` x `height`). `on_frame` is the
    /// animator callback, which calls [`Screen::frame`].
    ///
    /// # Safety
    /// `window` is a live Elementary window; call on the EFL main thread.
    pub unsafe fn new(
        window: *mut c_void,
        width: i32,
        height: i32,
        on_frame: EcoreTimerCb,
    ) -> Self {
        let evas = evas_object_evas_get(window);
        let art = Art::new();
        let rect = |color: u32| {
            let object = Obj::new(evas_object_rectangle_add(evas));
            evas_object_show(object.raw);
            let mut object = object;
            object.tint(premultiplied(color, 255));
            object
        };
        let mut background = rect(COLOR_PAGE);
        background.place(Bounds {
            x: 0,
            y: 0,
            w: width,
            h: height,
        });
        let mut rail = rect(COLOR_RAIL);
        // The rail's background and the app title never change.
        rail.place(Bounds {
            x: 0,
            y: 0,
            w: RAIL_W,
            h: height,
        });
        let mut rail_pill = Obj::new(sliced(evas, &art.pill, PILL_SLICE));
        rail_pill.tint(premultiplied(COLOR_PILL, 255));
        let mut rail_ring = Obj::new(sliced(evas, &art.ring, PANEL_SLICE));
        rail_ring.tint([0; 4]);
        let mut app_title = Obj::new(kit::text(evas, FONT, 36, "TV suite", false));
        app_title.move_to(48, TITLE_Y + 6);
        let mut title = Obj::new(kit::text(evas, FONT, 48, "", false));
        title.tint(premultiplied(COLOR_TEXT, 255));
        let mut status = Obj::new(kit::text(evas, FONT_REGULAR, 24, "", false));
        status.tint(premultiplied(COLOR_MUTED, 255));
        evas_object_text_ellipsis_set(status.raw, 1.0);
        let mut clip = rect(0xff_ff_ff);
        clip.place(Bounds {
            x: VIEW_X - 2 * RING_GAP,
            y: VIEW_TOP - 2 * RING_GAP,
            w: width - VIEW_X + 2 * RING_GAP,
            h: height - VIEW_TOP + 2 * RING_GAP,
        });
        let mut highlight = rect(0xff_ff_ff);
        highlight.tint([0; 4]);
        let mut accent = rect(COLOR_ACCENT);
        accent.tint([0; 4]);
        let mut ring = Obj::new(sliced(evas, &art.ring, PANEL_SLICE));
        ring.tint([0; 4]);
        for object in [&highlight, &accent, &ring] {
            evas_object_clip_set(object.raw, clip.raw);
        }
        let scratch_text = evas_object_text_add(evas);
        let scratch_block = evas_object_textblock_add(evas);
        let style = evas_textblock_style_new();
        evas_object_textblock_style_set(scratch_block, style);
        let mut screen = Self {
            window,
            evas,
            width,
            height,
            art,
            background,
            rail_labels: Vec::new(),
            rail_pill,
            rail_ring,
            title,
            status,
            clip,
            ring,
            highlight,
            accent,
            parts: Vec::new(),
            layout: Layout {
                parts: Vec::new(),
                focusables: Vec::new(),
                height: 0,
            },
            tabs: Vec::new(),
            tab: 0,
            in_rail: true,
            focus: 0,
            scroll: Glide::default(),
            ring_box: [Glide::default(); 4],
            ring_alpha: Glide::default(),
            highlight_alpha: Glide::default(),
            pill_y: Glide::default(),
            presence: 1.0,
            animator: ptr::null_mut(),
            last_frame: 0.0,
            scratch_text,
            scratch_block,
            style,
            field: ptr::null_mut(),
            on_frame,
        };
        screen.apply();
        screen
    }

    fn view_width(&self) -> i32 {
        self.width - VIEW_X - RIGHT_PAD
    }

    fn view_height(&self) -> i32 {
        self.height - VIEW_TOP - BOTTOM_PAD
    }

    /// The tabs (label, selectable); unselectable ones are dimmed.
    pub fn set_tabs(&mut self, tabs: &[(&str, bool)]) {
        for label in self.rail_labels.drain(..) {
            label.delete();
        }
        self.tabs = tabs
            .iter()
            .map(|(label, on)| (label.to_string(), *on))
            .collect();
        for (index, (label, on)) in self.tabs.iter().enumerate() {
            let mut object = Obj::new(unsafe { kit::text(self.evas, FONT, 28, label, false) });
            object.move_to(72, rail_item_y(index) + 14);
            object.tint(premultiplied(COLOR_TEXT, if *on { 255 } else { 90 }));
            self.rail_labels.push(object);
        }
        self.pill_y.jump(rail_item_y(self.tab) as f32);
        self.apply();
    }

    pub fn set_status(&mut self, text: &str) {
        unsafe {
            kit::set_text_value(self.status.raw, text);
            // The ellipsis is worked out on resize, not on a text change.
            evas_object_resize(self.status.raw, self.view_width(), 32);
        }
    }

    /// Shows `blocks` as the page of `tab`, titled `title`. Focus stays on
    /// the part with action `keep` if there is one. A new tab fades in from
    /// the top of the page; the same tab keeps its scroll position.
    pub fn set_page(&mut self, tab: usize, title: &str, blocks: &[Block<A>], keep: Option<&A>) {
        let new_tab = tab != self.tab;
        self.tab = tab;
        unsafe { kit::set_text_value(self.title.raw, title) };
        let layout = page::layout(blocks, self.view_width(), &*self);
        for part in self.parts.drain(..) {
            for (object, _) in part.objects {
                object.delete();
            }
        }
        self.parts = layout
            .parts
            .iter()
            .map(|placed| PartView {
                rect: placed.rect,
                part: placed.part.clone(),
                objects: Vec::new(),
                created: false,
            })
            .collect();
        self.focus = keep
            .and_then(|action| {
                layout
                    .focusables
                    .iter()
                    .position(|focusable| &focusable.action == action)
            })
            .unwrap_or(0);
        if layout.focusables.is_empty() {
            self.in_rail = true;
        }
        self.layout = layout;
        if new_tab {
            self.presence = 0.0;
            self.scroll.jump(0.0);
            self.pill_y.target = rail_item_y(tab) as f32;
        }
        let max = (self.layout.height - self.view_height()).max(0) as f32;
        self.scroll.target = self.scroll.target.min(max);
        self.scroll.now = self.scroll.now.min(max);
        self.aim(!new_tab);
        if new_tab {
            // The ring appears at its place instead of flying in.
            for glide in &mut self.ring_box {
                glide.now = glide.target;
            }
        }
        self.apply();
        self.start();
    }

    /// The focused part's action.
    pub fn focused(&self) -> Option<&A> {
        if self.in_rail {
            return None;
        }
        self.layout
            .focusables
            .get(self.focus)
            .map(|focusable| &focusable.action)
    }

    pub fn focus_rail(&mut self) {
        self.in_rail = true;
        self.aim(true);
        self.start();
    }

    pub fn key(&mut self, key: &str) -> Event<A> {
        if !self.field.is_null() {
            return Event::None;
        }
        let event = if self.in_rail {
            self.rail_key(key)
        } else {
            self.page_key(key)
        };
        self.aim(true);
        self.start();
        event
    }

    fn rail_key(&mut self, key: &str) -> Event<A> {
        let step = match key {
            "Up" => -1,
            "Down" => 1,
            "Right" | "Return" | "KP_Enter" => {
                if !self.layout.focusables.is_empty() {
                    self.in_rail = false;
                }
                return Event::None;
            }
            "XF86Back" | "Escape" | "BackSpace" => return Event::Back { in_rail: true },
            _ => return Event::None,
        };
        let mut next = self.tab as i32 + step;
        while next >= 0 && (next as usize) < self.tabs.len() {
            if self.tabs[next as usize].1 {
                return Event::Tab(next as usize);
            }
            next += step;
        }
        Event::None
    }

    fn page_key(&mut self, key: &str) -> Event<A> {
        let direction = match key {
            "Up" => Direction::Up,
            "Down" => Direction::Down,
            "Left" => Direction::Left,
            "Right" => Direction::Right,
            "Return" | "KP_Enter" | "Select" => {
                return self.focused().cloned().map_or(Event::None, Event::Action);
            }
            "XF86Back" | "Escape" | "BackSpace" => return Event::Back { in_rail: false },
            _ => return Event::None,
        };
        match page::neighbor(&self.layout.focusables, self.focus, direction) {
            Some(next) => self.focus = next,
            None if direction == Direction::Left => self.in_rail = true,
            None => {}
        }
        Event::None
    }

    /// Points the ring, highlight, scroll and rail at the current focus.
    fn aim(&mut self, keep_scroll: bool) {
        self.pill_y.target = rail_item_y(self.tab) as f32;
        let focusable = (!self.in_rail)
            .then(|| self.layout.focusables.get(self.focus))
            .flatten()
            .cloned();
        let Some(Focusable {
            rect, whole_row, ..
        }) = focusable
        else {
            self.ring_alpha.target = 0.0;
            self.highlight_alpha.target = 0.0;
            return;
        };
        let current = if keep_scroll {
            self.scroll.target as i32
        } else {
            0
        };
        self.scroll.target = page::scroll_to(
            rect,
            current,
            self.view_height(),
            self.layout.height,
            SCROLL_MARGIN,
        ) as f32;
        let grown = if whole_row {
            rect
        } else {
            Rect {
                x: rect.x - RING_GAP,
                y: rect.y - RING_GAP,
                w: rect.w + 2 * RING_GAP,
                h: rect.h + 2 * RING_GAP,
            }
        };
        for (glide, value) in self
            .ring_box
            .iter_mut()
            .zip([grown.x, grown.y, grown.w, grown.h])
        {
            glide.target = value as f32;
        }
        self.ring_alpha.target = if whole_row { 0.0 } else { 255.0 };
        self.highlight_alpha.target = if whole_row { 255.0 } else { 0.0 };
    }

    fn start(&mut self) {
        if self.animator.is_null() {
            self.last_frame = unsafe { ecore_time_get() };
            self.animator = unsafe { ecore_animator_add(self.on_frame, ptr::null()) };
        }
    }

    /// One animation frame; whether another is needed.
    pub fn frame(&mut self) -> bool {
        let now = unsafe { ecore_time_get() };
        let dt = (now - self.last_frame).clamp(0.0, 0.1);
        self.last_frame = now;
        let factor = (1.0 - (-dt * SPEED).exp()) as f32;
        let mut more = self.scroll.step(factor);
        for glide in &mut self.ring_box {
            more |= glide.step(factor);
        }
        more |= self.ring_alpha.step(factor);
        more |= self.highlight_alpha.step(factor);
        more |= self.pill_y.step(factor);
        if self.presence < 1.0 {
            self.presence = (self.presence + (dt / TRANSITION_SECONDS) as f32).min(1.0);
            more = true;
        }
        self.apply();
        if !more {
            self.animator = ptr::null_mut();
        }
        more
    }

    /// Maps the state onto the objects.
    fn apply(&mut self) {
        let eased = 1.0 - (1.0 - self.presence).powi(3);
        let rise = ((1.0 - eased) * TRANSITION_RISE).round() as i32;
        let alpha = (eased * 255.0).round() as i32;
        self.clip.tint([alpha; 4]);
        let scroll = self.scroll.now.round() as i32;
        let (top, bottom) = (
            scroll - CULL_MARGIN,
            scroll + self.view_height() + CULL_MARGIN,
        );
        let mut created = false;
        for index in 0..self.parts.len() {
            let rect = self.parts[index].rect;
            let visible = rect.y + rect.h >= top && rect.y <= bottom;
            if visible && !self.parts[index].created {
                self.create(index);
                created = true;
            }
            let part = &mut self.parts[index];
            let (x, y) = (VIEW_X + rect.x, VIEW_TOP + rect.y - scroll + rise);
            for (object, offset) in &mut part.objects {
                object.set_visible(visible);
                if visible {
                    object.place(Bounds {
                        x: x + offset.x,
                        y: y + offset.y,
                        w: offset.w,
                        h: offset.h,
                    });
                }
            }
        }
        if created {
            // Over everything just created.
            for object in [&self.highlight, &self.accent, &self.ring] {
                unsafe { evas_object_raise(object.raw) };
            }
        }
        let [x, y, w, h] = self.ring_box.map(|glide| glide.now.round() as i32);
        let (x, y) = (VIEW_X + x, VIEW_TOP + y - scroll + rise);
        self.ring.place(Bounds { x, y, w, h });
        self.ring.tint([self.ring_alpha.now.round() as i32; 4]);
        self.highlight.place(Bounds { x, y, w, h });
        let glow = (self.highlight_alpha.now * 0.07).round() as i32;
        self.highlight.tint([glow; 4]);
        self.accent.place(Bounds {
            x,
            y: y + 18,
            w: 5,
            h: (h - 36).max(0),
        });
        self.accent.tint(premultiplied(
            COLOR_ACCENT,
            self.highlight_alpha.now.round() as i32,
        ));
        // The rail.
        let pill_y = self.pill_y.now.round() as i32;
        let pill = Bounds {
            x: 32,
            y: pill_y,
            w: RAIL_W - 64,
            h: RAIL_ITEM_H,
        };
        self.rail_pill.place(pill);
        self.rail_pill.tint(premultiplied(
            if self.in_rail {
                COLOR_PILL_ACTIVE
            } else {
                COLOR_PILL
            },
            255,
        ));
        self.rail_ring.place(Bounds {
            x: pill.x - RING_GAP,
            y: pill.y - RING_GAP,
            w: pill.w + 2 * RING_GAP,
            h: pill.h + 2 * RING_GAP,
        });
        self.rail_ring.tint([if self.in_rail { 255 } else { 0 }; 4]);
        for (index, label) in self.rail_labels.iter_mut().enumerate() {
            let on = self.tabs.get(index).is_some_and(|tab| tab.1);
            let alpha = if index == self.tab {
                255
            } else if on {
                170
            } else {
                80
            };
            label.tint(premultiplied(COLOR_TEXT, alpha));
        }
        self.title.move_to(VIEW_X, TITLE_Y);
        self.status.place(Bounds {
            x: VIEW_X,
            y: STATUS_Y,
            w: self.view_width(),
            h: 32,
        });
        let _ = &self.background;
    }

    /// Creates the objects of part `index`.
    fn create(&mut self, index: usize) {
        let PartView { rect, part, .. } = &self.parts[index];
        let (w, h) = (rect.w, rect.h);
        let evas = self.evas;
        let mut objects: Vec<(Obj, Bounds)> = Vec::new();
        let at = |x: i32, y: i32, w: i32, h: i32| Bounds { x, y, w, h };
        unsafe {
            let panel = |color: u32, alpha: i32| {
                let mut object = Obj::new(sliced(evas, &self.art.panel, PANEL_SLICE));
                object.tint(premultiplied(color, alpha));
                object
            };
            let pill = |color: u32| {
                let mut object = Obj::new(sliced(evas, &self.art.pill, PILL_SLICE));
                object.tint(premultiplied(color, 255));
                object
            };
            let label = |font: &str, size: i32, value: &str, color: u32, alpha: i32| {
                let mut object = Obj::new(kit::text(evas, font, size, value, false));
                evas_object_text_ellipsis_set(object.raw, 1.0);
                object.tint(premultiplied(color, alpha));
                object
            };
            // A centred single line: its width is its own, at most `max`.
            let centred = |object: &Obj, max: i32, top: i32, height: i32| {
                let width = kit::advance(object.raw).min(max);
                at((w - width) / 2, top, width, height)
            };
            let icon = |path: &Option<String>,
                        title: &str,
                        side: i32,
                        behind: u32|
             -> Vec<(Obj, Bounds)> {
                let mut list = Vec::new();
                match path.as_deref().filter(|path| Path::new(path).is_file()) {
                    Some(path) => {
                        let image = evas_object_image_filled_add(evas);
                        evas_object_image_load_size_set(image, side * 2, side * 2);
                        let file = c_string(path);
                        evas_object_image_file_set(image, file.as_ptr(), ptr::null());
                        evas_object_image_smooth_scale_set(image, 1);
                        evas_object_show(image);
                        list.push((Obj::new(image), at(0, 0, side, side)));
                    }
                    None => {
                        list.push((panel(COLOR_RAISED, 255), at(0, 0, side, side)));
                        let letters =
                            label(FONT, side * 3 / 8, &page::initials(title), COLOR_TEXT, 255);
                        let width = kit::advance(letters.raw);
                        list.push((
                            letters,
                            at((side - width) / 2, side * 3 / 10, width, side / 2),
                        ));
                    }
                }
                let mut corners = Obj::new(kit::picture(evas, &self.art.corners, 256, 256));
                corners.tint(premultiplied(behind, 255));
                list.push((corners, at(0, 0, side, side)));
                list
            };
            match part {
                Part::Card => objects.push((panel(COLOR_CARD, 255), at(0, 0, w, h))),
                Part::Divider => {
                    let mut line = Obj::new(evas_object_rectangle_add(evas));
                    evas_object_show(line.raw);
                    line.tint(premultiplied(COLOR_DIVIDER, 255));
                    objects.push((line, at(0, 0, w, 1)));
                }
                Part::Section(title) => {
                    objects.push((label(FONT, 24, title, COLOR_MUTED, 255), at(0, 18, w, 32)));
                }
                Part::Tile { value, label: text } => {
                    objects.push((
                        label(FONT, 40, value, COLOR_TEXT, 255),
                        at(26, 14, w - 52, 50),
                    ));
                    objects.push((
                        label(FONT_REGULAR, 21, text, COLOR_MUTED, 255),
                        at(26, 64, w - 52, 28),
                    ));
                }
                Part::Icon { path, initials: _ } => {
                    let title = match &self.parts.get(index + 1).map(|next| &next.part) {
                        Some(Part::Text { title, .. }) => title.clone(),
                        _ => String::new(),
                    };
                    let behind = if w >= page::HEADER_ICON {
                        COLOR_PAGE
                    } else {
                        COLOR_CARD
                    };
                    objects.extend(icon(path, &title, w, behind));
                }
                Part::Text {
                    title,
                    subtitle,
                    title_size,
                    dim,
                } => {
                    let alpha = if *dim { 110 } else { 255 };
                    if subtitle.is_empty() {
                        let top = (h - title_size * 5 / 4) / 2;
                        objects.push((
                            label(FONT, *title_size, title, COLOR_TEXT, alpha),
                            at(0, top, w, title_size + 10),
                        ));
                    } else {
                        objects.push((
                            label(FONT, *title_size, title, COLOR_TEXT, alpha),
                            at(0, 0, w, title_size + 10),
                        ));
                        let subtitle = label(
                            FONT_REGULAR,
                            page::SUBTITLE_SIZE,
                            subtitle,
                            COLOR_MUTED,
                            alpha,
                        );
                        objects.push((subtitle, at(0, title_size + 12, w, 30)));
                    }
                }
                Part::Switch { on } => {
                    objects.push((
                        pill(if *on { COLOR_ACCENT } else { COLOR_TRACK }),
                        at(0, 0, w, h),
                    ));
                    let mut knob = Obj::new(kit::picture(evas, &self.art.knob, 64, 64));
                    knob.tint(premultiplied(0xff_ff_ff, 255));
                    let side = h - 10;
                    objects.push((knob, at(if *on { w - side - 5 } else { 5 }, 5, side, side)));
                }
                Part::Button { label: text, tone } => {
                    let (fill, ink) = match tone {
                        Tone::Normal => (COLOR_RAISED, COLOR_TEXT),
                        Tone::Primary => (COLOR_ACCENT, 0xff_ff_ff),
                        Tone::Danger => (COLOR_DANGER, COLOR_DANGER_TEXT),
                    };
                    objects.push((pill(fill), at(0, 0, w, h)));
                    let text = label(FONT, page::BUTTON_SIZE, text, ink, 255);
                    let place = centred(
                        &text,
                        w - 16,
                        (h - page::BUTTON_SIZE * 5 / 4) / 2,
                        page::BUTTON_SIZE + 8,
                    );
                    objects.push((text, place));
                }
                Part::Chip { label: text, on } => {
                    objects.push((
                        pill(if *on { COLOR_CHIP_ON } else { COLOR_CHIP }),
                        at(0, 0, w, h),
                    ));
                    let ink = if *on {
                        COLOR_CHIP_ON_TEXT
                    } else {
                        COLOR_CHIP_TEXT
                    };
                    let text = label(FONT_REGULAR, page::CHIP_SIZE, text, ink, 255);
                    let place = centred(
                        &text,
                        w - 8,
                        (h - page::CHIP_SIZE * 5 / 4) / 2,
                        page::CHIP_SIZE + 8,
                    );
                    objects.push((text, place));
                }
                Part::Progress { fraction, text } => {
                    objects.push((
                        label(FONT_REGULAR, 21, text, COLOR_TEXT, 255),
                        at(0, 2, w, 28),
                    ));
                    objects.push((pill(COLOR_TRACK), at(0, h - 16, w, 12)));
                    let filled = ((w as f32 * fraction.clamp(0.0, 1.0)) as i32).max(12);
                    objects.push((pill(COLOR_ACCENT), at(0, h - 16, filled, 12)));
                }
                Part::AppCard {
                    icon: path,
                    title,
                    subtitle,
                    badge,
                } => {
                    for (object, offset) in icon(path, title, page::CARD_ICON, COLOR_CARD) {
                        let offset = Bounds {
                            x: offset.x + (w - page::CARD_ICON) / 2,
                            y: offset.y + 24,
                            ..offset
                        };
                        objects.push((object, offset));
                    }
                    let name = label(FONT, 24, title, COLOR_TEXT, 255);
                    let place = centred(&name, w - 28, page::CARD_ICON + 40, 32);
                    objects.push((name, place));
                    let (line, color) = match badge {
                        Some(badge) => (badge.as_str(), COLOR_ACCENT),
                        None => (subtitle.as_str(), COLOR_MUTED),
                    };
                    let line = label(FONT_REGULAR, 20, line, color, 255);
                    let place = centred(&line, w - 28, page::CARD_ICON + 76, 28);
                    objects.push((line, place));
                }
                Part::Paragraph { markup, size } => {
                    let block = evas_object_textblock_add(evas);
                    let style = evas_textblock_style_new();
                    let text = c_string(&paragraph_style(*size));
                    evas_textblock_style_set(style, text.as_ptr());
                    evas_object_textblock_style_set(block, style);
                    let value = c_string(markup);
                    evas_object_textblock_text_markup_set(block, value.as_ptr());
                    evas_object_show(block);
                    objects.push((Obj::new(block), at(0, 0, w, h)));
                }
                Part::Field { text, placeholder } => {
                    objects.push((pill(COLOR_FIELD), at(0, 0, w, h)));
                    let (value, color) = if text.is_empty() {
                        (placeholder.as_str(), COLOR_MUTED)
                    } else {
                        (text.as_str(), COLOR_TEXT)
                    };
                    objects.push((
                        label(FONT_REGULAR, 24, value, color, 255),
                        at(30, (h - 30) / 2, w - 60, 34),
                    ));
                }
            }
            let is_card = matches!(part, Part::Card);
            for (object, _) in &objects {
                evas_object_clip_set(object.raw, self.clip.raw);
                if is_card {
                    // Cards go under everything else of the page.
                    evas_object_lower(object.raw);
                    evas_object_lower(self.background.raw);
                }
            }
        }
        let view = &mut self.parts[index];
        view.objects = objects;
        view.created = true;
    }

    /// Opens the TV's keyboard on a text field over the focused part, with
    /// `text`. `done` is called (from Elementary) when it is confirmed or
    /// loses focus; then call [`Screen::finish_field`].
    pub fn start_field(&mut self, text: &str, done: EvasSmartCb) {
        let Some(Focusable { rect, .. }) = self.layout.focusables.get(self.focus).cloned() else {
            return;
        };
        unsafe {
            let entry = elm_entry_add(self.window);
            elm_entry_single_line_set(entry, 1);
            elm_entry_scrollable_set(entry, 1);
            let value = c_string(&tv_proto::markup::escape(text));
            elm_object_part_text_set(entry, ptr::null(), value.as_ptr());
            let scroll = self.scroll.now.round() as i32;
            evas_object_move(entry, VIEW_X + rect.x + 16, VIEW_TOP + rect.y - scroll);
            evas_object_resize(entry, rect.w - 32, rect.h);
            for signal in [&b"activated\0"[..], &b"unfocused\0"[..]] {
                evas_object_smart_callback_add(
                    entry,
                    signal.as_ptr().cast(),
                    done,
                    ptr::null_mut(),
                );
            }
            evas_object_show(entry);
            elm_object_focus_set(entry, 1);
            self.field = entry;
        }
    }

    /// The field's text, once; the keyboard closes.
    pub fn finish_field(&mut self) -> Option<String> {
        if self.field.is_null() {
            return None;
        }
        let entry = std::mem::replace(&mut self.field, ptr::null_mut());
        let text = unsafe {
            let raw = elm_object_part_text_get(entry, ptr::null());
            let text = if raw.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(raw as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            evas_object_del(entry);
            evas_object_focus_set(self.window, 1);
            text
        };
        Some(
            text.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&"),
        )
    }
}

impl<A> Measure for Screen<A> {
    fn text_width(&self, text: &str, size: i32) -> i32 {
        unsafe {
            let font = c_string(FONT);
            evas_object_text_font_set(self.scratch_text, font.as_ptr(), size);
            kit::set_text_value(self.scratch_text, text);
            kit::advance(self.scratch_text)
        }
    }

    fn paragraph_height(&self, markup: &str, size: i32, width: i32) -> i32 {
        unsafe {
            let style = c_string(&paragraph_style(size));
            evas_textblock_style_set(self.style, style.as_ptr());
            evas_object_textblock_style_set(self.scratch_block, self.style);
            evas_object_resize(self.scratch_block, width, 1);
            let value = c_string(markup);
            evas_object_textblock_text_markup_set(self.scratch_block, value.as_ptr());
            let (mut w, mut h) = (0, 0);
            evas_object_textblock_size_formatted_get(self.scratch_block, &mut w, &mut h);
            h + 4
        }
    }
}

fn paragraph_style(size: i32) -> String {
    format!(
        "DEFAULT='font={FONT_REGULAR} font_size={size} color=#c8cfdaff wrap=word linesize={}' \
         b='+ font={FONT}' muted='+ color=#8a93a3ff' warn='+ color=#ffb74dff'",
        size * 3 / 2
    )
}

fn rail_item_y(index: usize) -> i32 {
    RAIL_ITEM_Y + index as i32 * (RAIL_ITEM_H + RAIL_ITEM_GAP)
}
