//! The whiteboard pipeline: `ruru-source` frame → dewarp → crop/downscale → JPEG.
//!
//! The perspective dewarp turns the keystoned camera frame into a head-on board
//! rectangle using the calibrated corners; uncalibrated, it cleanly falls back to no-warp.
//! Everything domain-specific about "the board" lives in this crate, never in `ruru-source`.

use crate::calibration::Calibration;
use crate::config::Config;
use crate::dewarp;
use image::codecs::jpeg::JpegEncoder;
use image::{ImageError, RgbImage};
use ruru_client::{ClientError, SourceClient};

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("talking to ruru-source: {0}")]
    Source(#[from] ClientError),

    #[error("decoding the source frame: {0}")]
    Decode(#[source] ImageError),

    #[error("encoding the result: {0}")]
    Encode(#[source] ImageError),

    #[error("invalid region {0}")]
    BadRegion(String),

    #[error("unknown mode `{0}` (expected `dewarped` or `high_contrast`)")]
    BadMode(String),

    #[error("calibration corners are degenerate (collinear or coincident)")]
    DegenerateCorners,
}

/// Post-dewarp enhancement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Dewarped colour (or raw, uncalibrated).
    #[default]
    Dewarped,
    /// Grayscale + contrast stretch — helps faint marker read as text.
    HighContrast,
}

impl Mode {
    pub fn parse(s: Option<&str>) -> Result<Mode, PipelineError> {
        match s {
            None | Some("dewarped") => Ok(Mode::Dewarped),
            Some("high_contrast") => Ok(Mode::HighContrast),
            Some(other) => Err(PipelineError::BadMode(other.to_owned())),
        }
    }
}

/// A crop rectangle in normalized (0..1) coordinates of the **dewarped board**.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Region {
    fn validate(&self) -> Result<(), PipelineError> {
        let Region { x, y, w, h } = *self;
        let in_unit = |v: f32| (0.0..=1.0).contains(&v);
        if !(in_unit(x) && in_unit(y) && in_unit(w) && in_unit(h)) {
            return Err(PipelineError::BadRegion(format!(
                "x/y/w/h must be within 0..1, got x={x} y={y} w={w} h={h}"
            )));
        }
        if w <= 0.0 || h <= 0.0 {
            return Err(PipelineError::BadRegion(format!(
                "w and h must be > 0, got w={w} h={h}"
            )));
        }
        if x + w > 1.0 + f32::EPSILON || y + h > 1.0 + f32::EPSILON {
            return Err(PipelineError::BadRegion(format!(
                "region extends past the board: x+w={} y+h={}",
                x + w,
                y + h
            )));
        }
        Ok(())
    }
}

/// An encoded JPEG plus its final dimensions.
#[derive(Debug, Clone)]
pub struct Encoded {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// `capture_raw` — the source frame, untouched. No decode/re-encode; the fast lossless path.
pub async fn capture_raw(source: &SourceClient) -> Result<Encoded, PipelineError> {
    let frame = source.snapshot().await?;
    Ok(Encoded {
        width: frame.width as u32,
        height: frame.height as u32,
        jpeg: frame.data.to_vec(),
    })
}

/// `capture_whiteboard` — dewarp to a head-on board and cap to the downscale budget.
pub async fn capture_whiteboard(
    source: &SourceClient,
    config: &Config,
    calibration: &Calibration,
    mode: Mode,
) -> Result<Encoded, PipelineError> {
    let raw = snapshot_rgb(source).await?;
    // Dewarp straight to the ceiling — no need to warp full-res then shrink.
    let long_edge = longest(&raw).min(config.pipeline.max_edge);
    let board = to_board(raw, calibration, long_edge)?;
    encode(&enhance(board, mode), config.pipeline.jpeg_quality)
}

/// `capture_region` — a normalized rectangle of the **dewarped board** at full effective
/// resolution (no downscale) — the escape hatch for dense handwriting.
pub async fn capture_region(
    source: &SourceClient,
    config: &Config,
    calibration: &Calibration,
    region: Region,
    mode: Mode,
) -> Result<Encoded, PipelineError> {
    region.validate()?;
    let raw = snapshot_rgb(source).await?;
    let long_edge = longest(&raw); // full effective resolution
    let board = to_board(raw, calibration, long_edge)?;
    let cropped = crop_normalized(&board, region);
    encode(&enhance(cropped, mode), config.pipeline.jpeg_quality)
}

/// Dewarp a fresh snapshot with **caller-supplied** corners/aspect (not the stored
/// calibration) — for the admin calibration preview. Sized to the downscale budget for a
/// snappy round-trip.
pub async fn dewarp_preview(
    source: &SourceClient,
    config: &Config,
    corners: [[f32; 2]; 4],
    aspect: [u32; 2],
) -> Result<Encoded, PipelineError> {
    let raw = snapshot_rgb(source).await?;
    let long_edge = longest(&raw).min(config.pipeline.max_edge);
    let out = dewarp::dewarp(&raw, &corners, aspect, long_edge)
        .ok_or(PipelineError::DegenerateCorners)?;
    encode(&out, config.pipeline.jpeg_quality)
}

/// One frame of the admin live preview: the source JPEG shrunk to `max_edge` and
/// re-encoded. Full 4K MJPEG at camera rate is ~100 Mbit/s per viewer and more than a
/// browser `<img>` can decode, so the stream flickers blank; corner placement only needs a
/// small image. CPU-bound: call from `spawn_blocking`.
pub fn preview_frame(jpeg: &[u8], max_edge: u32, quality: u8) -> Result<Vec<u8>, PipelineError> {
    let img = image::load_from_memory(jpeg)
        .map_err(PipelineError::Decode)?
        .to_rgb8();
    let long = longest(&img);
    let small = if long <= max_edge {
        img
    } else {
        let scale = max_edge as f64 / long as f64;
        let nw = ((img.width() as f64 * scale).round() as u32).max(1);
        let nh = ((img.height() as f64 * scale).round() as u32).max(1);
        // Triangle, not Lanczos: this runs several times a second and only feeds a preview.
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
    };
    Ok(encode(&small, quality)?.jpeg)
}

// --- internals ------------------------------------------------------------

async fn snapshot_rgb(source: &SourceClient) -> Result<RgbImage, PipelineError> {
    let frame = source.snapshot().await?;
    Ok(image::load_from_memory(&frame.data)
        .map_err(PipelineError::Decode)?
        .to_rgb8())
}

/// Dewarp to a head-on board of the calibrated aspect (longest edge `long_edge`), or —
/// uncalibrated — return the raw frame downscaled to `long_edge`.
fn to_board(
    raw: RgbImage,
    calibration: &Calibration,
    long_edge: u32,
) -> Result<RgbImage, PipelineError> {
    match &calibration.corners {
        Some(corners) => dewarp::dewarp(&raw, corners, calibration.aspect, long_edge)
            .ok_or(PipelineError::DegenerateCorners),
        None => Ok(downscale(raw, long_edge)),
    }
}

fn enhance(img: RgbImage, mode: Mode) -> RgbImage {
    match mode {
        Mode::Dewarped => img,
        Mode::HighContrast => dewarp::high_contrast(&img),
    }
}

fn longest(img: &RgbImage) -> u32 {
    img.width().max(img.height())
}

/// Downscale so the longest edge is at most `max_edge`, preserving aspect. Already within
/// budget ⇒ returned untouched.
fn downscale(img: RgbImage, max_edge: u32) -> RgbImage {
    let long = longest(&img);
    if long <= max_edge {
        return img;
    }
    let scale = max_edge as f64 / long as f64;
    let nw = ((img.width() as f64 * scale).round() as u32).max(1);
    let nh = ((img.height() as f64 * scale).round() as u32).max(1);
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3)
}

fn crop_normalized(img: &RgbImage, region: Region) -> RgbImage {
    let (iw, ih) = (img.width(), img.height());
    let px = (region.x * iw as f32).round() as u32;
    let py = (region.y * ih as f32).round() as u32;
    let pw = ((region.w * iw as f32).round() as u32)
        .min(iw.saturating_sub(px))
        .max(1);
    let ph = ((region.h * ih as f32).round() as u32)
        .min(ih.saturating_sub(py))
        .max(1);
    image::imageops::crop_imm(img, px, py, pw, ph).to_image()
}

fn encode(rgb: &RgbImage, quality: u8) -> Result<Encoded, PipelineError> {
    let (width, height) = (rgb.width(), rgb.height());
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, quality)
        .encode_image(rgb)
        .map_err(PipelineError::Encode)?;
    Ok(Encoded {
        jpeg,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_frames_are_shrunk_to_the_preview_budget() {
        let big = RgbImage::from_pixel(2000, 1000, image::Rgb([200, 200, 200]));
        let src = encode(&big, 80).unwrap().jpeg;
        let out = preview_frame(&src, 1280, 75).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (1280, 640));
    }

    fn uncalibrated() -> Calibration {
        Calibration {
            corners: None,
            aspect: [3, 2],
        }
    }

    fn calibrated() -> Calibration {
        Calibration {
            corners: Some([[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]]),
            aspect: [2, 1],
        }
    }

    #[test]
    fn mode_parsing() {
        assert_eq!(Mode::parse(None).unwrap(), Mode::Dewarped);
        assert_eq!(Mode::parse(Some("dewarped")).unwrap(), Mode::Dewarped);
        assert_eq!(
            Mode::parse(Some("high_contrast")).unwrap(),
            Mode::HighContrast
        );
        assert!(Mode::parse(Some("sepia")).is_err());
    }

    #[test]
    fn region_validation() {
        assert!(
            Region {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0
            }
            .validate()
            .is_ok()
        );
        assert!(
            Region {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5
            }
            .validate()
            .is_ok()
        );
        assert!(
            Region {
                x: 0.6,
                y: 0.0,
                w: 0.5,
                h: 0.5
            }
            .validate()
            .is_err()
        );
        assert!(
            Region {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.5
            }
            .validate()
            .is_err()
        );
        assert!(
            Region {
                x: -0.1,
                y: 0.0,
                w: 0.5,
                h: 0.5
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn uncalibrated_to_board_is_downscale_only() {
        let img = RgbImage::new(4000, 2000);
        let out = to_board(img, &uncalibrated(), 1568).unwrap();
        assert_eq!(out.width().max(out.height()), 1568);
    }

    #[test]
    fn calibrated_to_board_takes_the_aspect() {
        let img = RgbImage::new(1920, 1080);
        // aspect 2:1, long_edge 1000 → 1000x500
        let out = to_board(img, &calibrated(), 1000).unwrap();
        assert_eq!(out.dimensions(), (1000, 500));
    }

    #[test]
    fn crop_lands_where_asked() {
        let img = RgbImage::new(1000, 500);
        let out = crop_normalized(
            &img,
            Region {
                x: 0.25,
                y: 0.2,
                w: 0.5,
                h: 0.4,
            },
        );
        assert_eq!(out.dimensions(), (500, 200));
    }

    #[test]
    fn encode_produces_valid_jpeg() {
        let enc = encode(&RgbImage::new(64, 48), 85).unwrap();
        assert_eq!(&enc.jpeg[..2], &[0xFF, 0xD8], "JPEG SOI");
        assert_eq!((enc.width, enc.height), (64, 48));
    }
}
