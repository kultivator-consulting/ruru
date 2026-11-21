//! A synthetic camera that emits real JPEG.
//!
//! The payload is genuine MJPEG with `seq` and the capture timestamp burned into the
//! image, so downstream work (the `ruru-mcp` pipeline, the admin preview) can be built
//! and demoed with no Brio attached — and so a frame that arrives at the wrong consumer
//! or out of order is visible to the eye, not just to an assertion.

use super::font::{self, ADVANCE, GLYPH_H, GLYPH_W};
use super::{Camera, CameraError, CapturedFrame};
use bytes::Bytes;
use jpeg_encoder::{ColorType, Encoder};
use ruru_client::proto::{ControlDesc, FourCc};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct MockConfig {
    pub width: u16,
    pub height: u16,
    pub jpeg_quality: u8,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            jpeg_quality: 80,
        }
    }
}

/// Observable side effects, for tests that need to assert on device lifecycle.
#[derive(Debug, Default)]
pub struct MockStats {
    pub opens: AtomicU64,
    pub closes: AtomicU64,
    pub frames: AtomicU64,
}

impl MockStats {
    pub fn opens(&self) -> u64 {
        self.opens.load(Ordering::Relaxed)
    }
    pub fn closes(&self) -> u64 {
        self.closes.load(Ordering::Relaxed)
    }
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

pub struct MockCamera {
    cfg: MockConfig,
    open: bool,
    seq: u64,
    controls: BTreeMap<String, ControlDesc>,
    /// Reused between frames so steady-state capture doesn't churn the allocator.
    rgb: Vec<u8>,
    stats: Arc<MockStats>,
    /// Counts down on each `ensure_open`, failing while non-zero — exercises the
    /// close/back-off/reopen path without needing someone to unplug a camera.
    open_failures_remaining: u32,
}

impl MockCamera {
    pub fn new(cfg: MockConfig) -> Self {
        let pixels = cfg.width as usize * cfg.height as usize * 3;
        Self {
            cfg,
            open: false,
            seq: 0,
            controls: default_controls(),
            rgb: vec![0; pixels],
            stats: Arc::new(MockStats::default()),
            open_failures_remaining: 0,
        }
    }

    /// Fail the next `n` opens, then behave normally.
    pub fn with_open_failures(mut self, n: u32) -> Self {
        self.open_failures_remaining = n;
        self
    }

    /// Shared handle to this camera's lifecycle counters. Take it before handing the
    /// camera to the capture thread.
    pub fn stats(&self) -> Arc<MockStats> {
        Arc::clone(&self.stats)
    }

    fn render(&mut self, seq: u64, timestamp_us: u64) {
        let w = self.cfg.width as usize;
        let h = self.cfg.height as usize;

        // Brightness doubles as a visible proof that set_control reached the device.
        let gain = self
            .controls
            .get("brightness")
            .map(|c| c.value as f32 / 128.0)
            .unwrap_or(1.0);

        // Eight colour bars, scrolling so consecutive frames are distinguishable.
        const BARS: [[u8; 3]; 8] = [
            [192, 192, 192],
            [192, 192, 0],
            [0, 192, 192],
            [0, 192, 0],
            [192, 0, 192],
            [192, 0, 0],
            [0, 0, 192],
            [24, 24, 24],
        ];
        let bar_w = (w / BARS.len()).max(1);
        let scroll = (seq as usize * 4) % w;

        for y in 0..h {
            for x in 0..w {
                let bar = ((x + scroll) % w) / bar_w;
                let c = BARS[bar.min(BARS.len() - 1)];
                let i = (y * w + x) * 3;
                for (channel, level) in self.rgb[i..i + 3].iter_mut().zip(c) {
                    *channel = ((level as f32 * gain).round() as i32).clamp(0, 255) as u8;
                }
            }
        }

        // A sweeping vertical line: motion stays obvious even in a single still.
        let sweep = (seq as usize * 7) % w;
        for y in 0..h {
            let i = (y * w + sweep) * 3;
            self.rgb[i..i + 3].copy_from_slice(&[255, 255, 255]);
        }

        let scale = (w / 320).clamp(1, 6);
        let lines = [
            "RURU MOCK".to_string(),
            format!("SEQ {seq:012}"),
            format!("US {timestamp_us}"),
            format!("{}X{}", self.cfg.width, self.cfg.height),
        ];

        let pad = 4 * scale;
        let line_h = font::text_height(scale) + pad;
        let box_w = lines
            .iter()
            .map(|l| font::text_width(l, scale))
            .max()
            .unwrap_or(0)
            + pad * 2;
        let box_h = line_h * lines.len() + pad;

        // Dark backing so the text stays legible over any bar colour.
        for y in 0..box_h.min(h) {
            for x in 0..box_w.min(w) {
                let i = (y * w + x) * 3;
                for ch in 0..3 {
                    self.rgb[i + ch] /= 4;
                }
            }
        }

        for (n, line) in lines.iter().enumerate() {
            self.draw_text(line, pad, pad + n * line_h, scale, [255, 255, 255]);
        }
    }

    fn draw_text(&mut self, text: &str, x0: usize, y0: usize, scale: usize, colour: [u8; 3]) {
        let w = self.cfg.width as usize;
        let h = self.cfg.height as usize;

        for (n, ch) in text.chars().enumerate() {
            let g = font::glyph(ch);
            let gx = x0 + n * ADVANCE * scale;
            for (col, bits) in g.iter().enumerate().take(GLYPH_W) {
                for row in 0..GLYPH_H {
                    if bits & (1 << row) == 0 {
                        continue;
                    }
                    // Scale by filling a scale x scale block per lit bit.
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = gx + col * scale + dx;
                            let py = y0 + row * scale + dy;
                            if px >= w || py >= h {
                                continue;
                            }
                            let i = (py * w + px) * 3;
                            self.rgb[i..i + 3].copy_from_slice(&colour);
                        }
                    }
                }
            }
        }
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// The MX Brio's control set, transcribed from `v4l2-ctl -L` against a real device —
/// ranges, defaults and steps included, so the mock behaves like the hardware.
///
/// The one behaviour that is not just data is `flags=inactive`: three controls cannot be
/// set until a companion auto control is switched off first (see [`gate_for`]). That is
/// what makes locked-control *ordering* matter, and it is modelled here so the ordering
/// logic is regression-tested without a camera.
fn default_controls() -> BTreeMap<String, ControlDesc> {
    let c = |name: &str, value: i64, min: i64, max: i64, step: i64, default: i64| {
        (
            name.to_string(),
            ControlDesc {
                name: name.to_string(),
                value,
                min,
                max,
                step,
                default,
            },
        )
    };
    BTreeMap::from([
        // User controls
        c("brightness", 128, 0, 255, 1, 128),
        c("contrast", 128, 0, 255, 1, 128),
        c("saturation", 128, 0, 255, 1, 128),
        c("white_balance_automatic", 1, 0, 1, 1, 1),
        c("gain", 0, 0, 255, 1, 0),
        // menu: 0=Disabled, 1=50 Hz, 2=60 Hz. Defaults to 60 Hz — wrong for NZ mains,
        // which is exactly why config locks it to 1.
        c("power_line_frequency", 2, 0, 2, 1, 2),
        c("white_balance_temperature", 5000, 2800, 7500, 1, 5000), // inactive until wb auto off
        c("sharpness", 128, 0, 255, 1, 128),
        c("backlight_compensation", 1, 0, 1, 1, 1),
        // Camera controls
        // menu: only 1 (Manual) and 3 (Aperture Priority) are valid, despite the 0..=3
        // range. The real backend enumerates menu entries; the mock range-checks,
        // so it would wrongly accept 0/2. Documented, not fixed here.
        c("auto_exposure", 3, 0, 3, 1, 3),
        c("exposure_time_absolute", 156, 3, 2047, 1, 156), // inactive until auto_exposure=1
        c("exposure_dynamic_framerate", 0, 0, 1, 1, 0),
        c("pan_absolute", 0, -72000, 72000, 3600, 0),
        c("tilt_absolute", 0, -72000, 72000, 3600, 0),
        c("focus_absolute", 0, 0, 255, 1, 0), // inactive until focus auto off
        c("focus_automatic_continuous", 1, 0, 1, 1, 1),
        c("zoom_absolute", 100, 100, 400, 1, 100),
    ])
}

/// The Brio marks three controls `flags=inactive` until a companion auto control is
/// switched off. Returns `(gating control, the gate value that makes this one active)`.
///
/// This is why `[camera.controls]` order matters: the gate must be set before the gated
/// control, or the write silently does not take.
fn gate_for(name: &str) -> Option<(&'static str, i64)> {
    match name {
        "focus_absolute" => Some(("focus_automatic_continuous", 0)),
        "exposure_time_absolute" => Some(("auto_exposure", 1)), // 1 = Manual Mode
        "white_balance_temperature" => Some(("white_balance_automatic", 0)),
        _ => None,
    }
}

impl Camera for MockCamera {
    fn backend(&self) -> &'static str {
        "mock"
    }

    fn ensure_open(&mut self) -> Result<(), CameraError> {
        if self.open {
            return Ok(());
        }
        if self.open_failures_remaining > 0 {
            self.open_failures_remaining -= 1;
            return Err(CameraError::Open {
                device: "mock".into(),
                message: format!(
                    "simulated open failure ({} remaining)",
                    self.open_failures_remaining
                ),
            });
        }
        self.open = true;
        self.stats.opens.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn close(&mut self) {
        if self.open {
            self.open = false;
            self.stats.closes.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn grab_frame(&mut self) -> Result<CapturedFrame, CameraError> {
        if !self.open {
            return Err(CameraError::Capture("device is not open".into()));
        }

        let seq = self.seq;
        self.seq += 1;
        let timestamp_us = now_us();

        self.render(seq, timestamp_us);

        let mut jpeg = Vec::with_capacity(self.rgb.len() / 8);
        Encoder::new(&mut jpeg, self.cfg.jpeg_quality)
            .encode(&self.rgb, self.cfg.width, self.cfg.height, ColorType::Rgb)
            .map_err(|e| CameraError::Capture(format!("jpeg encode: {e}")))?;

        self.stats.frames.fetch_add(1, Ordering::Relaxed);

        Ok(CapturedFrame {
            width: self.cfg.width,
            height: self.cfg.height,
            fourcc: FourCc::MJPG,
            data: Bytes::from(jpeg),
            timestamp_us: Some(timestamp_us),
        })
    }

    fn query_controls(&mut self) -> Result<Vec<ControlDesc>, CameraError> {
        Ok(self.controls.values().cloned().collect())
    }

    fn set_control(&mut self, name: &str, value: i64) -> Result<ControlDesc, CameraError> {
        // Validate against the descriptor before touching it (immutable borrow first, so
        // the gate lookup below can also read self.controls).
        let desc = self
            .controls
            .get(name)
            .ok_or_else(|| CameraError::UnknownControl(name.to_owned()))?;

        if value < desc.min || value > desc.max {
            return Err(CameraError::ControlRange {
                name: name.to_owned(),
                value,
                min: desc.min,
                max: desc.max,
            });
        }
        let (min, step) = (desc.min, desc.step);

        // Inactive controls: the Brio accepts the write at the ioctl layer but silently
        // keeps the old value until the gate is off. Modelled as a no-op that returns the
        // *unchanged* descriptor — the insidious case, and the reason every caller must
        // re-read what it set rather than trust that the call succeeded.
        // (Real drivers vary here; some return EACCES instead.)
        if let Some((gate, active_value)) = gate_for(name)
            && self.controls.get(gate).map(|g| g.value) != Some(active_value)
        {
            return Ok(self.controls[name].clone());
        }

        // V4L2 snaps to the step grid rather than rejecting off-grid values — another
        // reason the returned descriptor, not the requested value, is the source of truth.
        let snapped = if step > 1 {
            min + ((value - min) / step) * step
        } else {
            value
        };
        let ctrl = self
            .controls
            .get_mut(name)
            .expect("existence checked above");
        ctrl.value = snapped;
        Ok(ctrl.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> MockCamera {
        MockCamera::new(MockConfig {
            width: 320,
            height: 240,
            jpeg_quality: 70,
        })
    }

    #[test]
    fn grabs_decodable_jpeg_and_counts_lifecycle() {
        let mut cam = small();
        let stats = cam.stats();

        cam.ensure_open().unwrap();
        cam.ensure_open().unwrap(); // idempotent
        assert_eq!(stats.opens(), 1);

        let frame = cam.grab_frame().unwrap();
        assert_eq!(frame.fourcc, FourCc::MJPG);
        assert_eq!((frame.width, frame.height), (320, 240));
        assert_eq!(&frame.data[..2], &[0xFF, 0xD8], "JPEG SOI");
        assert_eq!(
            &frame.data[frame.data.len() - 2..],
            &[0xFF, 0xD9],
            "JPEG EOI"
        );

        cam.close();
        cam.close(); // idempotent
        assert_eq!(stats.closes(), 1);
        assert_eq!(stats.frames(), 1);
    }

    #[test]
    fn frames_differ_between_captures() {
        let mut cam = small();
        cam.ensure_open().unwrap();
        let a = cam.grab_frame().unwrap();
        let b = cam.grab_frame().unwrap();
        assert_ne!(a.data, b.data, "burned-in seq must make frames distinct");
    }

    #[test]
    fn capture_without_open_fails() {
        assert!(small().grab_frame().is_err());
    }

    #[test]
    fn simulated_open_failures_then_recovery() {
        let mut cam = small().with_open_failures(2);
        let stats = cam.stats();
        assert!(cam.ensure_open().is_err());
        assert!(cam.ensure_open().is_err());
        cam.ensure_open().unwrap();
        assert_eq!(stats.opens(), 1);
    }

    #[test]
    fn set_control_validates_and_snaps_to_step() {
        let mut cam = small();

        let got = cam.set_control("brightness", 200).unwrap();
        assert_eq!(got.value, 200);
        assert_eq!(cam.get_control("brightness").unwrap().value, 200);

        // pan_absolute has step 3600, so 5000 lands on 3600.
        assert_eq!(cam.set_control("pan_absolute", 5000).unwrap().value, 3600);

        assert!(matches!(
            cam.set_control("brightness", 999),
            Err(CameraError::ControlRange { .. })
        ));
        assert!(matches!(
            cam.set_control("nonesuch", 1),
            Err(CameraError::UnknownControl(_))
        ));
    }

    /// The heart of the ordering bug: a gated control silently refuses to change until its
    /// auto companion is off, and the *only* signal is that a re-read shows the old value.
    #[test]
    fn a_gated_control_does_not_take_until_its_gate_is_off() {
        let mut cam = small();

        // Autofocus is on by default, so focus_absolute is inactive.
        let applied = cam.set_control("focus_absolute", 40).unwrap();
        assert_eq!(
            applied.value, 0,
            "write must be accepted but silently not take while autofocus is on"
        );
        assert_eq!(cam.get_control("focus_absolute").unwrap().value, 0);

        // Turn autofocus off, then the same write sticks.
        cam.set_control("focus_automatic_continuous", 0).unwrap();
        let applied = cam.set_control("focus_absolute", 40).unwrap();
        assert_eq!(applied.value, 40, "now active, the write should take");
        assert_eq!(cam.get_control("focus_absolute").unwrap().value, 40);
    }

    #[test]
    fn all_three_inactive_controls_gate_the_same_way() {
        let mut cam = small();

        // exposure_time_absolute is gated by auto_exposure == 1 (Manual Mode).
        assert_eq!(
            cam.set_control("exposure_time_absolute", 500)
                .unwrap()
                .value,
            156
        );
        cam.set_control("auto_exposure", 1).unwrap();
        assert_eq!(
            cam.set_control("exposure_time_absolute", 500)
                .unwrap()
                .value,
            500
        );

        // white_balance_temperature is gated by white_balance_automatic == 0.
        assert_eq!(
            cam.set_control("white_balance_temperature", 4000)
                .unwrap()
                .value,
            5000
        );
        cam.set_control("white_balance_automatic", 0).unwrap();
        assert_eq!(
            cam.set_control("white_balance_temperature", 4000)
                .unwrap()
                .value,
            4000
        );
    }

    /// Writes a real frame out so a human can confirm the overlay actually renders —
    /// the vendored font is hand-entered, and no assertion can tell legible text from
    /// well-formed garbage.
    ///
    /// `RURU_DUMP_FRAME=/tmp/frame.jpg cargo test -p ruru-source dump_sample_frame -- --ignored`
    #[test]
    #[ignore = "writes a file; run deliberately"]
    fn dump_sample_frame() {
        let path =
            std::env::var("RURU_DUMP_FRAME").expect("set RURU_DUMP_FRAME to the output path");
        let mut cam = MockCamera::new(MockConfig {
            width: 1280,
            height: 720,
            jpeg_quality: 85,
        });
        cam.ensure_open().unwrap();
        for _ in 0..41 {
            cam.grab_frame().unwrap();
        }
        let frame = cam.grab_frame().unwrap();
        std::fs::write(&path, &frame.data).unwrap();
        eprintln!("wrote {} bytes to {path}", frame.data.len());
    }

    #[test]
    fn brightness_visibly_changes_the_image() {
        let mut cam = small();
        cam.ensure_open().unwrap();
        let bright = cam.grab_frame().unwrap();
        cam.set_control("brightness", 8).unwrap();
        let dark = cam.grab_frame().unwrap();
        // A near-black frame compresses far smaller than full colour bars.
        assert!(
            dark.data.len() < bright.data.len(),
            "brightness={} produced {} bytes, expected well under {}",
            8,
            dark.data.len(),
            bright.data.len()
        );
    }
}
