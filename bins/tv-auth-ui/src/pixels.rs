//! The dock's pictures, drawn in memory: rounded shapes, the focus ring, the
//! glow and the shadow, and finished icons. Pixels are EFL's layout:
//! premultiplied ARGB, one `u32` each, rows of `width` pixels.
//!
//! Every shape is a rounded rectangle measured with its signed distance
//! (negative inside), so edges are antialiased and the ring, glow and icons
//! share one corner rule.

/// Corner radius as a share of an icon's side.
pub const RADIUS_SHARE: f32 = 0.16;

/// Signed distance from (`x`, `y`) to a rounded box of half size `half_w` by
/// `half_h` and corner `radius`, centred on the origin.
fn rounded_box(x: f32, y: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = x.abs() - (half_w - radius);
    let qy = y.abs() - (half_h - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

/// Calls `shade(distance)` for every pixel of a `width` x `height` canvas
/// against the box `inset` pixels inside its edge; `shade` returns the
/// premultiplied pixel.
fn paint(
    width: usize,
    height: usize,
    inset: f32,
    radius: f32,
    shade: impl Fn(f32) -> u32,
) -> Vec<u32> {
    let (half_w, half_h) = (width as f32 / 2.0 - inset, height as f32 / 2.0 - inset);
    let mut pixels = vec![0; width * height];
    for y in 0..height {
        for x in 0..width {
            let distance = rounded_box(
                x as f32 + 0.5 - width as f32 / 2.0,
                y as f32 + 0.5 - height as f32 / 2.0,
                half_w,
                half_h,
                radius,
            );
            pixels[y * width + x] = shade(distance);
        }
    }
    pixels
}

/// Straight `rgb` at `alpha` (0..1) as one premultiplied pixel.
pub fn premultiply(rgb: u32, alpha: f32) -> u32 {
    let alpha = alpha.clamp(0.0, 1.0);
    let channel = |shift: u32| (((rgb >> shift) & 0xff) as f32 * alpha).round() as u32;
    ((alpha * 255.0).round() as u32) << 24 | channel(16) << 16 | channel(8) << 8 | channel(0)
}

/// How much of a pixel at `distance` lies inside the edge.
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

/// A filled rounded rectangle with a `border`-wide line of `border_rgb` at
/// `border_alpha` along its inside edge.
pub fn panel(
    side: usize,
    radius: f32,
    rgb: u32,
    alpha: f32,
    border: f32,
    border_rgb: u32,
    border_alpha: f32,
) -> Vec<u32> {
    paint(side, side, 0.0, radius, |distance| {
        let inside = coverage(distance);
        // The share of the pixel in the border band, drawn over the fill.
        let band = (inside - coverage(distance + border)) * border_alpha;
        let fill = unpack(premultiply(rgb, alpha * inside));
        let line = unpack(premultiply(border_rgb, band));
        pack([0, 1, 2, 3].map(|channel| line[channel] + fill[channel] * (1.0 - band)))
    })
}

/// A white ring `width` wide just outside a rounded square of `inner` side
/// (the image is `inner + 2 * width` square).
pub fn ring(inner: usize, width: usize) -> Vec<u32> {
    let side = inner + 2 * width;
    let radius = inner as f32 * RADIUS_SHARE;
    paint(side, side, width as f32, radius, |distance| {
        premultiply(
            0xff_ff_ff,
            coverage(distance - width as f32) - coverage(distance),
        )
    })
}

/// A soft white halo around a rounded square of `inner` side, fading out over
/// `spread` pixels (the image is `inner + 2 * spread` square). Tint it with
/// the object's colour.
pub fn glow(inner: usize, spread: usize, strength: f32) -> Vec<u32> {
    let side = inner + 2 * spread;
    let radius = inner as f32 * RADIUS_SHARE;
    paint(side, side, spread as f32, radius, |distance| {
        let t = (distance / spread as f32).clamp(0.0, 1.0);
        premultiply(0xff_ff_ff, strength * (1.0 - t) * (1.0 - t))
    })
}

/// A black shadow for a nine-sliced panel: a rounded square of corner
/// `radius`, blurred over `blur` pixels, in an image of `2 * (radius + blur)
/// + 1` square (slice it at `radius + blur`).
pub fn shadow(radius: usize, blur: usize, strength: f32) -> Vec<u32> {
    let side = 2 * (radius + blur) + 1;
    paint(side, side, blur as f32, radius as f32, |distance| {
        let t = ((distance + blur as f32 / 2.0) / blur as f32).clamp(0.0, 1.0);
        premultiply(0, strength * (1.0 - t * t * (3.0 - 2.0 * t)))
    })
}

/// A filled circle of `rgb`, `side` across.
pub fn dot(side: usize, rgb: u32) -> Vec<u32> {
    let radius = side as f32 / 2.0;
    paint(side, side, 0.0, radius, |distance| {
        premultiply(rgb, coverage(distance))
    })
}

/// A white chevron `side` square, pointing up or down, lines `width` thick.
pub fn chevron(side: usize, up: bool, width: f32) -> Vec<u32> {
    let s = side as f32;
    // Two strokes meeting in the middle: from (0.25, 0.62) over (0.5, 0.38) to (0.75, 0.62), flipped for down.
    let flip = |y: f32| if up { y } else { 1.0 - y };
    let points =
        [(0.22, flip(0.64)), (0.5, flip(0.36)), (0.78, flip(0.64))].map(|(x, y)| (x * s, y * s));
    let mut pixels = vec![0; side * side];
    for y in 0..side {
        for x in 0..side {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let distance = points
                .windows(2)
                .map(|pair| segment_distance(p, pair[0], pair[1]))
                .fold(f32::MAX, f32::min);
            pixels[y * side + x] = premultiply(0xff_ff_ff, coverage(distance - width / 2.0));
        }
    }
    pixels
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Premultiplied channels as floats, for averaging.
fn unpack(pixel: u32) -> [f32; 4] {
    [24, 16, 8, 0].map(|shift| ((pixel >> shift) & 0xff) as f32)
}

fn pack(channels: [f32; 4]) -> u32 {
    let [a, r, g, b] = channels.map(|value| value.round().clamp(0.0, 255.0) as u32);
    a << 24 | r.min(a) << 16 | g.min(a) << 8 | b.min(a)
}

/// Area-average resampling of one line of `source` to `length` samples
/// (`length` at most the source's).
fn shrink_line(source: &[[f32; 4]], length: usize) -> Vec<[f32; 4]> {
    let scale = source.len() as f32 / length as f32;
    (0..length)
        .map(|index| {
            let (start, end) = (index as f32 * scale, (index + 1) as f32 * scale);
            let mut sum = [0.0; 4];
            let mut position = start.floor() as usize;
            while (position as f32) < end && position < source.len() {
                let weight = (end.min(position as f32 + 1.0) - start.max(position as f32)).max(0.0);
                for (total, value) in sum.iter_mut().zip(source[position]) {
                    *total += value * weight;
                }
                position += 1;
            }
            sum.map(|total| total / scale)
        })
        .collect()
}

/// `source` (`width` x `height`) shrunk to `new_width` x `new_height`, each
/// at most the original.
pub fn shrink(
    source: &[u32],
    width: usize,
    height: usize,
    new_width: usize,
    new_height: usize,
) -> Vec<u32> {
    let rows: Vec<Vec<[f32; 4]>> = source
        .chunks(width)
        .take(height)
        .map(|row| {
            shrink_line(
                &row.iter().map(|pixel| unpack(*pixel)).collect::<Vec<_>>(),
                new_width,
            )
        })
        .collect();
    let mut out = vec![0; new_width * new_height];
    for x in 0..new_width {
        let column: Vec<[f32; 4]> = rows.iter().map(|row| row[x]).collect();
        for (y, value) in shrink_line(&column, new_height).into_iter().enumerate() {
            out[y * new_width + x] = pack(value);
        }
    }
    out
}

/// `source` over `target` at (`x`, `y`); both premultiplied.
pub fn over(
    target: &mut [u32],
    target_width: usize,
    source: &[u32],
    width: usize,
    x: usize,
    y: usize,
) {
    for (row, line) in source.chunks(width).enumerate() {
        for (column, pixel) in line.iter().enumerate() {
            let index = (y + row) * target_width + x + column;
            let Some(below) = target.get(index).copied() else {
                continue;
            };
            let (top, under) = (unpack(*pixel), unpack(below));
            let keep = 1.0 - top[0] / 255.0;
            target[index] = pack([0, 1, 2, 3].map(|channel| top[channel] + under[channel] * keep));
        }
    }
}

/// Cuts rounded corners (`radius`) into a `side`-square picture.
pub fn round_corners(pixels: &mut [u32], side: usize, radius: f32) {
    let half = side as f32 / 2.0;
    for y in 0..side {
        for x in 0..side {
            let distance = rounded_box(
                x as f32 + 0.5 - half,
                y as f32 + 0.5 - half,
                half,
                half,
                radius,
            );
            let keep = coverage(distance);
            if keep < 1.0 {
                let channels = unpack(pixels[y * side + x]);
                pixels[y * side + x] = pack(channels.map(|value| value * keep));
            }
        }
    }
}

/// The square an icon of `width` x `height` is finished at: its larger side,
/// at most `max`.
pub fn icon_side(width: usize, height: usize, max: usize) -> usize {
    width.max(height).min(max).max(1)
}

/// A finished icon: `source` fitted whole into a square of `side` (see
/// [`icon_side`]) over `backdrop` (straight RGB; shows behind transparent or
/// non-square art), with rounded corners.
pub fn icon(source: &[u32], width: usize, height: usize, side: usize, backdrop: u32) -> Vec<u32> {
    let scale = side as f32 / width.max(height) as f32;
    let (fit_width, fit_height) = (
        ((width as f32 * scale).round() as usize).clamp(1, side),
        ((height as f32 * scale).round() as usize).clamp(1, side),
    );
    let fitted = if (fit_width, fit_height) == (width, height) {
        source[..width * height].to_vec()
    } else {
        shrink(source, width, height, fit_width, fit_height)
    };
    let mut out = vec![premultiply(backdrop, 1.0); side * side];
    over(
        &mut out,
        side,
        &fitted,
        fit_width,
        (side - fit_width) / 2,
        (side - fit_height) / 2,
    );
    round_corners(&mut out, side, side as f32 * RADIUS_SHARE);
    out
}

/// Whether a picture's visible pixels are mostly light (it needs a dark
/// backdrop).
pub fn is_light(pixels: &[u32]) -> bool {
    let (mut light, mut total) = (0.0_f32, 0.0_f32);
    for pixel in pixels {
        let [a, r, g, b] = unpack(*pixel);
        if a > 0.0 {
            // Premultiplied channels sum to alpha-weighted straight ones.
            light += 0.299 * r + 0.587 * g + 0.114 * b;
            total += a;
        }
    }
    total > 0.0 && light / total > 0.5
}

/// The colour an icon's glow takes: its dominant hue (straight RGB),
/// weighting each pixel by opacity and saturation and skipping dark ones;
/// `None` for a blank or grey picture.
pub fn dominant(pixels: &[u32]) -> Option<u32> {
    let (mut sum, mut total) = ([0.0_f64; 3], 0.0_f64);
    for pixel in pixels.iter().step_by((pixels.len() / 4096).max(1)) {
        let [a, r, g, b] = unpack(*pixel).map(f64::from);
        if a == 0.0 {
            continue;
        }
        let straight = [r, g, b].map(|channel| (channel * 255.0 / a).min(255.0));
        let (high, low) = (
            straight.iter().copied().fold(0.0, f64::max),
            straight.iter().copied().fold(255.0, f64::min),
        );
        let saturation = if high > 0.0 { (high - low) / high } else { 0.0 };
        let weight = a / 255.0 * saturation * if high > 40.0 { 1.0 } else { 0.1 };
        for (accumulated, channel) in sum.iter_mut().zip(straight) {
            *accumulated += channel * weight;
        }
        total += weight;
    }
    // Mostly grey (YouTube's white tile, a dark logo): the default glow.
    if total < pixels.len().min(4096) as f64 * 0.05 {
        return None;
    }
    let [r, g, b] = sum.map(|channel| (channel / total).round() as u32);
    Some(r << 16 | g << 8 | b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(pixel: u32) -> u32 {
        pixel >> 24
    }

    #[test]
    fn rounded_corners_are_transparent_and_the_middle_opaque() {
        let side = 100;
        let picture = panel(side, 24.0, 0x2a_33_46, 1.0, 2.0, 0xff_ff_ff, 0.22);
        assert_eq!(alpha(picture[0]), 0);
        assert_eq!(alpha(picture[50 * side + 50]), 255);
        assert_eq!(picture[50 * side + 50], 0xff_2a_33_46);
        // The border line is lighter than the fill.
        assert!(picture[50 * side] & 0xff > 0x46);
    }

    #[test]
    fn ring_is_hollow() {
        let picture = ring(100, 3);
        let side = 106;
        assert_eq!(alpha(picture[53 * side + 53]), 0);
        assert_eq!(alpha(picture[53 * side + 1]), 255);
    }

    #[test]
    fn glow_fades_outwards() {
        let picture = glow(100, 30, 0.5);
        let side = 160;
        let at = |x: usize| alpha(picture[80 * side + x]);
        assert!(at(30) > at(15) && at(15) > at(2));
        assert_eq!(at(0), 0);
    }

    #[test]
    fn shrinking_averages_premultiplied_pixels() {
        let source = [0xff_ff_00_00, 0x00_00_00_00, 0xff_ff_00_00, 0x00_00_00_00];
        let half = shrink(&source, 2, 2, 1, 1);
        assert_eq!(half, vec![0x80_80_00_00]);
    }

    #[test]
    fn wide_icons_are_fitted_whole_on_the_backdrop() {
        let source = vec![0xff_00_ff_00; 40 * 20];
        let finished = icon(&source, 40, 20, icon_side(40, 20, 224), 0x30_30_30);
        assert_eq!(finished.len(), 40 * 40);
        assert_eq!(finished[5 * 40 + 20], 0xff_30_30_30); // above the art: backdrop
        assert_eq!(finished[20 * 40 + 20], 0xff_00_ff_00); // the art
        assert_eq!(alpha(finished[0]), 0); // a rounded corner
    }

    #[test]
    fn dominant_colour_ignores_grey() {
        let red = vec![0xff_e5_09_14; 100];
        assert_eq!(dominant(&red), Some(0xe5_09_14));
        let grey = vec![0xff_80_80_80; 100];
        assert_eq!(dominant(&grey), None);
    }

    #[test]
    fn light_and_dark_art() {
        assert!(is_light(&[0xff_ff_ff_ff, 0]));
        assert!(!is_light(&[0xff_00_1f_33]));
    }

    #[test]
    fn chevron_points_the_right_way() {
        let up = chevron(24, true, 3.0);
        let down = chevron(24, false, 3.0);
        // The apex: high in the up chevron, low in the down one.
        assert!(alpha(up[9 * 24 + 12]) > 0 && alpha(up[15 * 24 + 12]) == 0);
        assert!(alpha(down[15 * 24 + 12]) > 0 && alpha(down[9 * 24 + 12]) == 0);
    }
}
