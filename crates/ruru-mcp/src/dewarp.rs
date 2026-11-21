//! Perspective dewarp — a hand-rolled homography with bilinear sampling.
//!
//! The camera sits off-axis, so the board is keystoned. Given the four board corners
//! (marked during calibration, normalized 0..1 over the raw frame, order **TL, TR, BR,
//! BL**) and the measured board aspect, this maps the source quadrilateral to a head-on
//! rectangle: solve the 4-point homography (DLT, an exact 8×8 system), then inverse-map
//! every output pixel back into the source with bilinear sampling.
//!
//! Domain-specific, so it lives in `ruru-mcp` — `ruru-source` never learns what a
//! whiteboard is.

use image::{Rgb, RgbImage};

/// A 3×3 projective transform (row-major), with the bottom-right element normalized to 1.
pub struct Homography([f64; 9]);

impl Homography {
    /// The homography mapping the four `from` points to the four `to` points exactly.
    ///
    /// `None` if the four `from` points are degenerate (collinear / coincident), which
    /// makes the 8×8 system singular.
    pub fn from_points(from: &[[f64; 2]; 4], to: &[[f64; 2]; 4]) -> Option<Homography> {
        // For each correspondence (x,y) -> (u,v), with h8 = 1:
        //   h0 x + h1 y + h2 - h6 x u - h7 y u = u
        //   h3 x + h4 y + h5 - h6 x v - h7 y v = v
        let mut a = [[0.0f64; 8]; 8];
        let mut b = [0.0f64; 8];
        for i in 0..4 {
            let (x, y) = (from[i][0], from[i][1]);
            let (u, v) = (to[i][0], to[i][1]);
            let r = 2 * i;
            a[r] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u];
            b[r] = u;
            a[r + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v];
            b[r + 1] = v;
        }
        let h = solve8(a, b)?;
        Some(Homography([
            h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0,
        ]))
    }

    /// Apply the transform to a point, returning the projected (x, y).
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let h = &self.0;
        let w = h[6] * x + h[7] * y + h[8];
        (
            (h[0] * x + h[1] * y + h[2]) / w,
            (h[3] * x + h[4] * y + h[5]) / w,
        )
    }
}

/// Gaussian elimination with partial pivoting for an 8×8 system. `None` if singular.
fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let mut pivot = col;
        for r in (col + 1)..8 {
            if a[r][col].abs() > a[pivot][col].abs() {
                pivot = r;
            }
        }
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);

        for r in 0..8 {
            if r == col {
                continue;
            }
            let factor = a[r][col] / a[col][col];
            if factor != 0.0 {
                // Subtract the pivot row from row r; the index spans both rows, so an
                // iterator would need the pivot copied out — the range is clearer here.
                let pivot = a[col];
                #[allow(clippy::needless_range_loop)]
                for c in col..8 {
                    a[r][c] -= factor * pivot[c];
                }
                b[r] -= factor * b[col];
            }
        }
    }
    let mut x = [0.0f64; 8];
    for i in 0..8 {
        x[i] = b[i] / a[i][i];
    }
    Some(x)
}

/// Output dimensions for a board of `aspect` (w : h), sized so the longest edge ≈
/// `long_edge`. Warping to the wrong aspect stretches text, so the aspect must be measured.
pub fn output_dims(aspect: [u32; 2], long_edge: u32) -> (u32, u32) {
    let aw = aspect[0].max(1) as f64;
    let ah = aspect[1].max(1) as f64;
    let long_edge = long_edge.max(1);
    if aw >= ah {
        let h = ((long_edge as f64) * ah / aw).round().max(1.0) as u32;
        (long_edge, h)
    } else {
        let w = ((long_edge as f64) * aw / ah).round().max(1.0) as u32;
        (w, long_edge)
    }
}

/// Dewarp `src` to a head-on rectangle of the measured `aspect`, sized so its longest edge
/// ≈ `long_edge`. `corners` are normalized 0..1 over `src`, order TL, TR, BR, BL.
///
/// `None` only if the corners are degenerate; callers treat that as "not calibrated".
pub fn dewarp(
    src: &RgbImage,
    corners: &[[f32; 2]; 4],
    aspect: [u32; 2],
    long_edge: u32,
) -> Option<RgbImage> {
    let sw = src.width() as f64;
    let sh = src.height() as f64;
    let source_quad: [[f64; 2]; 4] =
        std::array::from_fn(|i| [corners[i][0] as f64 * sw, corners[i][1] as f64 * sh]);

    let (ow, oh) = output_dims(aspect, long_edge);
    // Output rectangle corners in the same TL, TR, BR, BL order.
    let out_rect: [[f64; 2]; 4] = [
        [0.0, 0.0],
        [(ow - 1) as f64, 0.0],
        [(ow - 1) as f64, (oh - 1) as f64],
        [0.0, (oh - 1) as f64],
    ];

    // H maps output-pixel coordinates back into the source, so we can inverse-sample.
    let h = Homography::from_points(&out_rect, &source_quad)?;

    let mut out = RgbImage::new(ow, oh);
    for oy in 0..oh {
        for ox in 0..ow {
            let (fx, fy) = h.apply(ox as f64, oy as f64);
            out.put_pixel(ox, oy, bilinear(src, fx, fy));
        }
    }
    Some(out)
}

/// Bilinear sample with edge clamping.
fn bilinear(src: &RgbImage, x: f64, y: f64) -> Rgb<u8> {
    let w = src.width() as i64;
    let h = src.height() as i64;
    let x = x.clamp(0.0, (w - 1) as f64);
    let y = y.clamp(0.0, (h - 1) as f64);
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let dx = x - x0 as f64;
    let dy = y - y0 as f64;

    let at = |xx: i64, yy: i64| src.get_pixel(xx as u32, yy as u32).0;
    let (p00, p10, p01, p11) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));

    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] as f64 * (1.0 - dx) + p10[c] as f64 * dx;
        let bot = p01[c] as f64 * (1.0 - dx) + p11[c] as f64 * dx;
        out[c] = (top * (1.0 - dy) + bot * dy).round().clamp(0.0, 255.0) as u8;
    }
    Rgb(out)
}

/// `high_contrast` enhancement: luma + a robust (2nd–98th percentile) contrast stretch,
/// returned as grayscale-in-RGB. Helps faint dry-erase marker read as text.
pub fn high_contrast(img: &RgbImage) -> RgbImage {
    let mut luma: Vec<u8> = Vec::with_capacity((img.width() * img.height()) as usize);
    for p in img.pixels() {
        let [r, g, b] = p.0;
        // Rec. 601 luma.
        let y = (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64).round() as u8;
        luma.push(y);
    }

    // Percentile bounds from a 256-bin histogram.
    let mut hist = [0u32; 256];
    for &y in &luma {
        hist[y as usize] += 1;
    }
    let total = luma.len() as u32;
    let lo = percentile(&hist, total, 0.02);
    let hi = percentile(&hist, total, 0.98).max(lo + 1);
    let scale = 255.0 / (hi - lo) as f64;

    let mut out = RgbImage::new(img.width(), img.height());
    for (i, p) in out.pixels_mut().enumerate() {
        let stretched = (((luma[i] as i32 - lo as i32) as f64) * scale)
            .round()
            .clamp(0.0, 255.0) as u8;
        *p = Rgb([stretched, stretched, stretched]);
    }
    out
}

fn percentile(hist: &[u32; 256], total: u32, frac: f64) -> u8 {
    let target = (total as f64 * frac) as u32;
    let mut cum = 0u32;
    for (v, &count) in hist.iter().enumerate() {
        cum += count;
        if cum >= target {
            return v as u8;
        }
    }
    255
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORNERS_TL_TR_BR_BL: [[f64; 2]; 4] = [[0.0, 0.0], [10.0, 0.0], [10.0, 8.0], [0.0, 8.0]];

    #[test]
    fn homography_maps_its_corners_exactly() {
        // An arbitrary keystoned source quad.
        let src = [[3.0, 2.0], [19.0, 1.0], [17.0, 15.0], [1.0, 14.0]];
        let h = Homography::from_points(&CORNERS_TL_TR_BR_BL, &src).unwrap();
        for i in 0..4 {
            let (x, y) = h.apply(CORNERS_TL_TR_BR_BL[i][0], CORNERS_TL_TR_BR_BL[i][1]);
            assert!(
                (x - src[i][0]).abs() < 1e-6,
                "corner {i} x: {x} vs {}",
                src[i][0]
            );
            assert!(
                (y - src[i][1]).abs() < 1e-6,
                "corner {i} y: {y} vs {}",
                src[i][1]
            );
        }
    }

    #[test]
    fn identity_correspondence_is_identity() {
        let h = Homography::from_points(&CORNERS_TL_TR_BR_BL, &CORNERS_TL_TR_BR_BL).unwrap();
        let (x, y) = h.apply(4.0, 5.0);
        assert!((x - 4.0).abs() < 1e-9 && (y - 5.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_corners_return_none() {
        let flat = [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0]]; // collinear
        assert!(Homography::from_points(&CORNERS_TL_TR_BR_BL, &flat).is_none());
    }

    #[test]
    fn output_dims_follow_aspect() {
        assert_eq!(output_dims([3, 2], 1500), (1500, 1000));
        assert_eq!(output_dims([2, 3], 1500), (1000, 1500));
        assert_eq!(output_dims([1, 1], 1000), (1000, 1000));
    }

    #[test]
    fn full_frame_corners_reproduce_the_image() {
        // A 4x4 gradient; corners = the whole frame; aspect matches → dewarp ≈ identity.
        let mut img = RgbImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                img.put_pixel(x, y, Rgb([(x * 60) as u8, (y * 60) as u8, 0]));
            }
        }
        let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let out = dewarp(&img, &corners, [1, 1], 4).unwrap();
        assert_eq!(out.dimensions(), (4, 4));
        // Corners land on the original corner pixels.
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0]);
        assert_eq!(out.get_pixel(3, 0).0, [180, 0, 0]);
        assert_eq!(out.get_pixel(0, 3).0, [0, 180, 0]);
    }

    #[test]
    fn high_contrast_stretches_a_dull_image() {
        // A low-contrast image (values 100..108) should stretch toward the full range.
        let mut img = RgbImage::new(8, 8);
        for (i, p) in img.pixels_mut().enumerate() {
            let v = 100 + (i % 8) as u8;
            *p = Rgb([v, v, v]);
        }
        let out = high_contrast(&img);
        let vals: Vec<u8> = out.pixels().map(|p| p.0[0]).collect();
        let (min, max) = (*vals.iter().min().unwrap(), *vals.iter().max().unwrap());
        assert!(max - min > 200, "expected a wide spread, got {min}..{max}");
    }
}
