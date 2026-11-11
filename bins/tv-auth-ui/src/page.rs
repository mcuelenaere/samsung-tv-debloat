//! The manager's screens as data (host-tested): a page is a list of blocks
//! (sections, status tiles, card groups of rows with switches, buttons or
//! progress, chips, a grid of app cards, a header, paragraphs). `layout`
//! places them, `neighbor` moves focus between the focusable parts by
//! geometry, and `scroll_to` keeps the focused part in view. `screen.rs`
//! turns the result into evas objects.

/// Text measurements the layout needs from the renderer.
pub trait Measure {
    fn text_width(&self, text: &str, size: i32) -> i32;
    fn paragraph_height(&self, markup: &str, size: i32, width: i32) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Primary,
    Danger,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control<A> {
    /// The whole row is focusable; OK runs the action.
    Switch { on: bool, action: A },
    Button {
        label: String,
        tone: Tone,
        action: A,
    },
    /// Replaces a row's buttons while a change runs.
    Progress { fraction: f32, text: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row<A> {
    pub icon: Option<String>,
    pub title: String,
    pub subtitle: String,
    pub controls: Vec<Control<A>>,
    /// Chips under the row (host groups).
    pub chips: Vec<Chip<A>>,
    /// Dimmed (chips of a switched-off row, inactive entries).
    pub dim: bool,
}

impl<A> Row<A> {
    pub fn text(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            icon: None,
            title: title.into(),
            subtitle: subtitle.into(),
            controls: Vec::new(),
            chips: Vec::new(),
            dim: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chip<A> {
    pub label: String,
    pub on: bool,
    pub action: A,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Card<A> {
    pub icon: Option<String>,
    pub title: String,
    pub subtitle: String,
    /// A short highlighted note ("Update", "installed").
    pub badge: Option<String>,
    pub action: A,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block<A> {
    Section(String),
    /// (value, label) pairs side by side.
    Tiles(Vec<(String, String)>),
    /// Rows in one rounded card.
    Rows(Vec<Row<A>>),
    Chips(Vec<Chip<A>>),
    Grid(Vec<Card<A>>),
    /// Big icon, title and a line of facts.
    Header {
        icon: Option<String>,
        title: String,
        facts: String,
    },
    /// Wrapped text (evas textblock markup).
    Paragraph {
        markup: String,
        size: i32,
    },
    /// A focusable text field (the store's search) and buttons after it.
    Toolbar {
        field: (String, String, A),
        buttons: Vec<(String, A)>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn center(&self) -> (f32, f32) {
        (
            self.x as f32 + self.w as f32 / 2.0,
            self.y as f32 + self.h as f32 / 2.0,
        )
    }
}

/// One thing to draw, in page coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// A rounded card background.
    Card,
    /// A thin line between rows of a card.
    Divider,
    Section(String),
    Tile {
        value: String,
        label: String,
    },
    Icon {
        path: Option<String>,
        initials: String,
    },
    /// Title and subtitle; `width` is what they may use.
    Text {
        title: String,
        subtitle: String,
        title_size: i32,
        dim: bool,
    },
    Switch {
        on: bool,
    },
    Button {
        label: String,
        tone: Tone,
    },
    Chip {
        label: String,
        on: bool,
    },
    Progress {
        fraction: f32,
        text: String,
    },
    /// A grid card: icon, title, subtitle, badge.
    AppCard {
        icon: Option<String>,
        title: String,
        subtitle: String,
        badge: Option<String>,
    },
    Paragraph {
        markup: String,
        size: i32,
    },
    Field {
        text: String,
        placeholder: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub part: Part,
    pub rect: Rect,
}

/// Something focus can land on.
#[derive(Clone, Debug, PartialEq)]
pub struct Focusable<A> {
    pub rect: Rect,
    pub action: A,
    /// Rows highlight as a whole; buttons, chips and cards get a ring.
    pub whole_row: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout<A> {
    pub parts: Vec<Placed>,
    pub focusables: Vec<Focusable<A>>,
    pub height: i32,
}

// ---- Metrics, in pixels at 1920x1080 ----
pub const SECTION_H: i32 = 56;
pub const TILE_H: i32 = 104;
pub const TILE_GAP: i32 = 16;
pub const ROW_H: i32 = 96;
pub const ROW_PAD_X: i32 = 28;
pub const ROW_ICON: i32 = 64;
pub const TITLE_SIZE: i32 = 30;
pub const SUBTITLE_SIZE: i32 = 22;
pub const SWITCH_W: i32 = 76;
pub const SWITCH_H: i32 = 42;
pub const BUTTON_H: i32 = 56;
pub const BUTTON_PAD: i32 = 30;
pub const BUTTON_SIZE: i32 = 24;
pub const BUTTON_GAP: i32 = 14;
pub const PROGRESS_W: i32 = 360;
pub const CHIP_H: i32 = 48;
pub const CHIP_PAD: i32 = 22;
pub const CHIP_SIZE: i32 = 22;
pub const CHIP_GAP: i32 = 12;
pub const GRID_COLUMNS: i32 = 5;
pub const GRID_GAP: i32 = 24;
pub const CARD_H: i32 = 232;
pub const CARD_ICON: i32 = 112;
pub const HEADER_ICON: i32 = 176;
pub const BLOCK_GAP: i32 = 12;
pub const FIELD_W: i32 = 560;

pub fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

pub fn button_width(measure: &dyn Measure, label: &str) -> i32 {
    measure.text_width(label, BUTTON_SIZE) + 2 * BUTTON_PAD
}

/// Places `blocks` in a column `width` wide.
pub fn layout<A: Clone>(blocks: &[Block<A>], width: i32, measure: &dyn Measure) -> Layout<A> {
    let mut out = Layout {
        parts: Vec::new(),
        focusables: Vec::new(),
        height: 0,
    };
    let mut y = 0;
    let place = |out: &mut Layout<A>, part: Part, rect: Rect| out.parts.push(Placed { part, rect });
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            y += BLOCK_GAP;
        }
        match block {
            Block::Section(title) => {
                place(
                    &mut out,
                    Part::Section(title.clone()),
                    Rect {
                        x: 4,
                        y,
                        w: width - 8,
                        h: SECTION_H,
                    },
                );
                y += SECTION_H - BLOCK_GAP;
            }
            Block::Tiles(tiles) => {
                let count = tiles.len().max(1) as i32;
                let tile_w = (width - TILE_GAP * (count - 1)) / count;
                for (column, (value, label)) in tiles.iter().enumerate() {
                    let x = column as i32 * (tile_w + TILE_GAP);
                    place(
                        &mut out,
                        Part::Card,
                        Rect {
                            x,
                            y,
                            w: tile_w,
                            h: TILE_H,
                        },
                    );
                    let part = Part::Tile {
                        value: value.clone(),
                        label: label.clone(),
                    };
                    place(
                        &mut out,
                        part,
                        Rect {
                            x,
                            y,
                            w: tile_w,
                            h: TILE_H,
                        },
                    );
                }
                y += TILE_H;
            }
            Block::Rows(rows) => {
                let top = y;
                let card_at = out.parts.len();
                place(&mut out, Part::Card, Rect::default());
                for (row_index, row) in rows.iter().enumerate() {
                    if row_index > 0 {
                        place(
                            &mut out,
                            Part::Divider,
                            Rect {
                                x: ROW_PAD_X,
                                y,
                                w: width - 2 * ROW_PAD_X,
                                h: 1,
                            },
                        );
                    }
                    y = layout_row(&mut out, row, y, width, measure);
                }
                out.parts[card_at].rect = Rect {
                    x: 0,
                    y: top,
                    w: width,
                    h: y - top,
                };
            }
            Block::Chips(chips) => {
                y = layout_chips(&mut out, chips, 0, y, width, false, measure);
            }
            Block::Grid(cards) => {
                let card_w = (width - GRID_GAP * (GRID_COLUMNS - 1)) / GRID_COLUMNS;
                for (index, card) in cards.iter().enumerate() {
                    let (column, line) = (index as i32 % GRID_COLUMNS, index as i32 / GRID_COLUMNS);
                    let rect = Rect {
                        x: column * (card_w + GRID_GAP),
                        y: y + line * (CARD_H + GRID_GAP),
                        w: card_w,
                        h: CARD_H,
                    };
                    place(&mut out, Part::Card, rect);
                    let part = Part::AppCard {
                        icon: card.icon.clone(),
                        title: card.title.clone(),
                        subtitle: card.subtitle.clone(),
                        badge: card.badge.clone(),
                    };
                    place(&mut out, part, rect);
                    out.focusables.push(Focusable {
                        rect,
                        action: card.action.clone(),
                        whole_row: false,
                    });
                }
                let lines = (cards.len() as i32 + GRID_COLUMNS - 1) / GRID_COLUMNS;
                y += (lines * (CARD_H + GRID_GAP) - GRID_GAP).max(0);
            }
            Block::Header { icon, title, facts } => {
                let rect = Rect {
                    x: 0,
                    y,
                    w: HEADER_ICON,
                    h: HEADER_ICON,
                };
                place(
                    &mut out,
                    Part::Icon {
                        path: icon.clone(),
                        initials: initials(title),
                    },
                    rect,
                );
                let text = Part::Text {
                    title: title.clone(),
                    subtitle: facts.clone(),
                    title_size: 48,
                    dim: false,
                };
                let x = HEADER_ICON + 36;
                place(
                    &mut out,
                    text,
                    Rect {
                        x,
                        y: y + HEADER_ICON / 2 - 50,
                        w: width - x,
                        h: 100,
                    },
                );
                y += HEADER_ICON;
            }
            Block::Paragraph { markup, size } => {
                let h = measure.paragraph_height(markup, *size, width);
                place(
                    &mut out,
                    Part::Paragraph {
                        markup: markup.clone(),
                        size: *size,
                    },
                    Rect {
                        x: 0,
                        y,
                        w: width,
                        h,
                    },
                );
                y += h;
            }
            Block::Toolbar { field, buttons } => {
                let (text, placeholder, action) = field;
                let rect = Rect {
                    x: 0,
                    y,
                    w: FIELD_W,
                    h: BUTTON_H + 8,
                };
                place(
                    &mut out,
                    Part::Field {
                        text: text.clone(),
                        placeholder: placeholder.clone(),
                    },
                    rect,
                );
                out.focusables.push(Focusable {
                    rect,
                    action: action.clone(),
                    whole_row: false,
                });
                let mut x = FIELD_W + BUTTON_GAP;
                for (label, action) in buttons {
                    let w = button_width(measure, label);
                    let rect = Rect {
                        x,
                        y: y + 4,
                        w,
                        h: BUTTON_H,
                    };
                    place(
                        &mut out,
                        Part::Button {
                            label: label.clone(),
                            tone: Tone::Normal,
                        },
                        rect,
                    );
                    out.focusables.push(Focusable {
                        rect,
                        action: action.clone(),
                        whole_row: false,
                    });
                    x += w + BUTTON_GAP;
                }
                y += BUTTON_H + 8;
            }
        }
    }
    out.height = y;
    out
}

/// One row of a card at `y`; returns the y below it.
fn layout_row<A: Clone>(
    out: &mut Layout<A>,
    row: &Row<A>,
    y: i32,
    width: i32,
    measure: &dyn Measure,
) -> i32 {
    let mut text_x = ROW_PAD_X;
    if row.icon.is_some() {
        let rect = Rect {
            x: ROW_PAD_X,
            y: y + (ROW_H - ROW_ICON) / 2,
            w: ROW_ICON,
            h: ROW_ICON,
        };
        out.parts.push(Placed {
            part: Part::Icon {
                path: row.icon.clone(),
                initials: initials(&row.title),
            },
            rect,
        });
        text_x += ROW_ICON + 24;
    }
    // Controls from the right edge leftwards.
    let mut right = width - ROW_PAD_X;
    let mut focusables = Vec::new();
    let switch_row = matches!(row.controls.as_slice(), [Control::Switch { .. }]);
    for control in row.controls.iter().rev() {
        let (part, w, h, action) = match control {
            Control::Switch { on, action } => {
                (Part::Switch { on: *on }, SWITCH_W, SWITCH_H, Some(action))
            }
            Control::Button {
                label,
                tone,
                action,
            } => (
                Part::Button {
                    label: label.clone(),
                    tone: *tone,
                },
                button_width(measure, label),
                BUTTON_H,
                Some(action),
            ),
            Control::Progress { fraction, text } => (
                Part::Progress {
                    fraction: *fraction,
                    text: text.clone(),
                },
                PROGRESS_W,
                BUTTON_H,
                None,
            ),
        };
        let rect = Rect {
            x: right - w,
            y: y + (ROW_H - h) / 2,
            w,
            h,
        };
        out.parts.push(Placed { part, rect });
        if let Some(action) = action.filter(|_| !switch_row) {
            focusables.push(Focusable {
                rect,
                action: action.clone(),
                whole_row: false,
            });
        }
        right -= w + BUTTON_GAP;
    }
    focusables.reverse();
    let text = Part::Text {
        title: row.title.clone(),
        subtitle: row.subtitle.clone(),
        title_size: TITLE_SIZE,
        dim: row.dim,
    };
    out.parts.push(Placed {
        part: text,
        rect: Rect {
            x: text_x,
            y: y + 14,
            w: right - text_x,
            h: ROW_H - 28,
        },
    });
    if switch_row {
        if let [Control::Switch { action, .. }] = row.controls.as_slice() {
            out.focusables.push(Focusable {
                rect: Rect {
                    x: 0,
                    y,
                    w: width,
                    h: ROW_H,
                },
                action: action.clone(),
                whole_row: true,
            });
        }
    }
    out.focusables.extend(focusables);
    let mut bottom = y + ROW_H;
    if !row.chips.is_empty() {
        bottom = layout_chips(
            out,
            &row.chips,
            ROW_PAD_X,
            bottom - 8,
            width - ROW_PAD_X,
            row.dim,
            measure,
        ) + 20;
    }
    bottom
}

/// Chips wrapped into lines from `x0` to `right`; returns the y below them.
fn layout_chips<A: Clone>(
    out: &mut Layout<A>,
    chips: &[Chip<A>],
    x0: i32,
    y: i32,
    right: i32,
    dim: bool,
    measure: &dyn Measure,
) -> i32 {
    let (mut x, mut y) = (x0, y);
    for chip in chips {
        let w = measure.text_width(&chip.label, CHIP_SIZE) + 2 * CHIP_PAD;
        if x > x0 && x + w > right {
            x = x0;
            y += CHIP_H + CHIP_GAP;
        }
        let rect = Rect { x, y, w, h: CHIP_H };
        out.parts.push(Placed {
            part: Part::Chip {
                label: chip.label.clone(),
                on: chip.on && !dim,
            },
            rect,
        });
        out.focusables.push(Focusable {
            rect,
            action: chip.action.clone(),
            whole_row: false,
        });
        x += w + CHIP_GAP;
    }
    y + CHIP_H
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// The focusable nearest to `current` in `direction`: along that axis
/// first, then the least sideways. `None` at the edge.
pub fn neighbor<A>(
    focusables: &[Focusable<A>],
    current: usize,
    direction: Direction,
) -> Option<usize> {
    let from = focusables.get(current)?.rect;
    let (fx, fy) = from.center();
    let overlap = |a0: i32, a1: i32, b0: i32, b1: i32| a0.max(b0) < a1.min(b1);
    let mut best: Option<(f32, usize)> = None;
    for (index, candidate) in focusables.iter().enumerate() {
        if index == current {
            continue;
        }
        let to = candidate.rect;
        let (tx, ty) = to.center();
        let (along, across, aligned) = match direction {
            // Up and Down compare left edges: from a wide row the first
            // chip, between grid lines the same column.
            Direction::Down => (
                to.y - (from.y + from.h) + 1,
                (to.x - from.x) as f32,
                overlap(from.x, from.x + from.w, to.x, to.x + to.w),
            ),
            // Up and Down compare left edges: from a wide row the first
            // chip, between grid lines the same column.
            Direction::Up => (
                from.y - (to.y + to.h) + 1,
                (to.x - from.x) as f32,
                overlap(from.x, from.x + from.w, to.x, to.x + to.w),
            ),
            Direction::Right => (
                to.x - (from.x + from.w) + 1,
                ty - fy,
                ty >= from.y as f32 && ty <= (from.y + from.h) as f32,
            ),
            Direction::Left => (
                from.x - (to.x + to.w) + 1,
                ty - fy,
                ty >= from.y as f32 && ty <= (from.y + from.h) as f32,
            ),
        };
        // Strictly beyond the current one in that direction.
        let beyond = match direction {
            Direction::Down => ty > fy && to.y >= from.y + from.h / 2,
            Direction::Up => ty < fy && to.y + to.h <= from.y + from.h / 2,
            Direction::Right => tx > fx && to.x >= from.x + from.w / 2,
            Direction::Left => tx < fx && to.x + to.w <= from.x + from.w / 2,
        };
        if !beyond {
            continue;
        }
        // Left and Right stay in their line: only to something whose middle
        // is level with the current one.
        if matches!(direction, Direction::Left | Direction::Right) && !aligned {
            continue;
        }
        let sideways = if aligned { 0.0 } else { across.abs() };
        let score = along.max(0) as f32 + 3.0 * sideways + 0.01 * across.abs();
        if best.is_none_or(|(score_best, _)| score < score_best) {
            best = Some((score, index));
        }
    }
    best.map(|(_, index)| index)
}

/// The scroll offset that shows `rect` in a `view` high window over a page
/// `height` high: unchanged if it is fully visible with `margin` to spare,
/// else as close to the middle as the page allows.
/// The focusable closest to `rect`: nearest line first, then column.
pub fn nearest<A>(focusables: &[Focusable<A>], rect: Rect) -> Option<usize> {
    focusables
        .iter()
        .enumerate()
        .min_by_key(|(_, focusable)| {
            (
                (focusable.rect.y - rect.y).abs(),
                (focusable.rect.x - rect.x).abs(),
            )
        })
        .map(|(index, _)| index)
}

pub fn scroll_to(rect: Rect, current: i32, view: i32, height: i32, margin: i32) -> i32 {
    let max = (height - view).max(0);
    if rect.y - margin >= current && rect.y + rect.h + margin <= current + view {
        return current.min(max);
    }
    (rect.y + rect.h / 2 - view / 2).clamp(0, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl Measure for Fixed {
        fn text_width(&self, text: &str, size: i32) -> i32 {
            text.len() as i32 * size / 2
        }
        fn paragraph_height(&self, markup: &str, size: i32, width: i32) -> i32 {
            (markup.len() as i32 * size / 2 / width + 1) * size * 3 / 2
        }
    }

    fn switch(title: &str, action: u32) -> Row<u32> {
        let mut row = Row::text(title, "note");
        row.controls.push(Control::Switch { on: true, action });
        row
    }

    #[test]
    fn nearest_prefers_the_same_line() {
        let at = |x, y| Focusable {
            rect: Rect { x, y, w: 10, h: 10 },
            action: 0u32,
            whole_row: false,
        };
        let focusables = [at(0, 0), at(500, 100), at(0, 130)];
        assert_eq!(
            nearest(
                &focusables,
                Rect {
                    x: 600,
                    y: 100,
                    w: 10,
                    h: 10
                }
            ),
            Some(1)
        );
        assert_eq!(
            nearest(
                &focusables,
                Rect {
                    x: 0,
                    y: 125,
                    w: 10,
                    h: 10
                }
            ),
            Some(2)
        );
        assert_eq!(nearest::<u32>(&[], Rect::default()), None);
    }

    #[test]
    fn switch_rows_focus_whole_and_chips_follow() {
        let mut hosts = switch("Block hosts", 1);
        hosts.chips = (0..12)
            .map(|i| Chip {
                label: format!("Group {i}"),
                on: true,
                action: 10 + i,
            })
            .collect();
        let page = vec![
            Block::Section("Blocking".into()),
            Block::Rows(vec![hosts, switch("Updates", 2)]),
        ];
        let layout = layout(&page, 1400, &Fixed);
        let actions: Vec<u32> = layout.focusables.iter().map(|f| f.action).collect();
        assert_eq!(actions[0], 1);
        assert!(layout.focusables[0].whole_row);
        assert_eq!(*actions.last().unwrap(), 2);
        // Down from the row lands on the first chip; Down from the chips on the next row.
        assert_eq!(neighbor(&layout.focusables, 0, Direction::Down), Some(1));
        let last_chip = actions.iter().position(|&a| a == 21).unwrap();
        assert_eq!(
            neighbor(&layout.focusables, last_chip, Direction::Down).map(|i| actions[i]),
            Some(2)
        );
        assert_eq!(
            neighbor(&layout.focusables, 1, Direction::Right).map(|i| actions[i]),
            Some(11)
        );
        // The chips wrapped onto more than one line.
        let chip_rows: std::collections::BTreeSet<i32> = layout.focusables[1..last_chip + 1]
            .iter()
            .map(|f| f.rect.y)
            .collect();
        assert!(chip_rows.len() > 1);
        assert_eq!(neighbor(&layout.focusables, 0, Direction::Left), None);
    }

    #[test]
    fn buttons_in_a_row_are_separate_and_ordered() {
        let mut row = Row::text("Kodi", "v21");
        row.controls = vec![
            Control::Button {
                label: "Open".into(),
                tone: Tone::Primary,
                action: 1,
            },
            Control::Button {
                label: "Remove".into(),
                tone: Tone::Danger,
                action: 2,
            },
        ];
        let layout = layout(&[Block::Rows(vec![row])], 1400, &Fixed);
        assert_eq!(layout.focusables.len(), 2);
        assert_eq!(layout.focusables[0].action, 1);
        assert!(layout.focusables[0].rect.x < layout.focusables[1].rect.x);
        assert_eq!(neighbor(&layout.focusables, 0, Direction::Right), Some(1));
        assert_eq!(neighbor(&layout.focusables, 1, Direction::Left), Some(0));
    }

    #[test]
    fn grid_moves_by_column_and_line() {
        let cards: Vec<Card<u32>> = (0..12)
            .map(|i| Card {
                icon: None,
                title: format!("App {i}"),
                subtitle: String::new(),
                badge: None,
                action: i,
            })
            .collect();
        let layout = layout(&[Block::Grid(cards)], 1400, &Fixed);
        let f = &layout.focusables;
        assert_eq!(neighbor(f, 0, Direction::Right), Some(1));
        assert_eq!(neighbor(f, 0, Direction::Down), Some(GRID_COLUMNS as usize));
        assert_eq!(neighbor(f, 7, Direction::Up), Some(2));
        assert_eq!(neighbor(f, 4, Direction::Right), None);
        // From the last line's short end, Up goes straight up.
        assert_eq!(neighbor(f, 11, Direction::Up), Some(6));
        assert_eq!(layout.height, 3 * CARD_H + 2 * GRID_GAP);
    }

    #[test]
    fn scrolling_keeps_focus_in_view() {
        let rect = Rect {
            x: 0,
            y: 900,
            w: 100,
            h: 100,
        };
        assert_eq!(scroll_to(rect, 0, 600, 3000, 40), 650);
        // Already visible: unchanged.
        assert_eq!(scroll_to(rect, 600, 600, 3000, 40), 600);
        // Never past the end.
        assert_eq!(scroll_to(Rect { y: 2950, ..rect }, 0, 600, 3000, 40), 2400);
        assert_eq!(scroll_to(Rect { y: 0, ..rect }, 300, 600, 3000, 40), 0);
    }

    #[test]
    fn initials_skip_symbols() {
        assert_eq!(initials("/dev/null on TV"), "DO");
        assert_eq!(initials("Kodi"), "K");
    }
}
