use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub source: SourceConfig,
    pub camera: CameraConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub socket_path: PathBuf,

    /// Group to own the socket. The source socket's trust boundary is the filesystem —
    /// there is no application token on this hop — so when this is set and the chown
    /// fails, startup aborts rather than serving on a socket with the wrong owner.
    /// Leave unset in dev, where the socket simply keeps the process's own group.
    #[serde(default)]
    pub socket_group: Option<String>,

    /// Hold the device open regardless of subscriber count.
    #[serde(default)]
    pub pin: bool,

    /// Master gate at startup. When false, capture is refused until something enables it.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// How long the device stays open after the last client detaches.
    #[serde(default = "default_idle_grace")]
    pub idle_grace_secs: u64,

    /// Per-subscriber frame queue depth. A subscriber that falls this far behind skips
    /// to the newest frame — the capture loop is never held up by a slow consumer.
    #[serde(default = "default_broadcast_capacity")]
    pub broadcast_capacity: usize,

    /// Give up on a snapshot if no frame arrives in this long (e.g. the device won't open).
    #[serde(default = "default_snapshot_timeout")]
    pub snapshot_timeout_secs: u64,

    /// Drop a connection that connects but never sends a request.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,

    /// Where runtime control overrides are persisted (a small `name → value` JSON written
    /// when a control is set with `persist`). Layered over `[camera.controls]` on every open,
    /// so operator tuning survives a restart. The deployed config points this at a writable
    /// state dir (`/var/lib/ruru-source/…`) because the daemon's own config under `/etc` is
    /// read-only to it. Unset (dev/mock) means control tuning stays runtime-only.
    #[serde(default)]
    pub controls_state_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Mock,
    /// Real V4L2 capture (`/dev/v4l/by-id/…`).
    V4l2,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraConfig {
    #[serde(default)]
    pub backend: Backend,

    /// Must be a `/dev/v4l/by-id/…` path. The Brio exposes two video nodes and renumbers
    /// them on replug, so `/dev/video0` is never a stable reference to anything.
    #[serde(default)]
    pub device: Option<PathBuf>,

    #[serde(default = "default_fourcc")]
    pub fourcc: String,

    pub width: u16,
    pub height: u16,

    #[serde(default = "default_fps")]
    pub fps: u32,

    /// Frames discarded after each open. UVC devices need a few captures before exposure
    /// and white balance settle; without this the first frame out is garbage.
    #[serde(default = "default_warmup")]
    pub warmup_frames: u32,

    /// Controls reapplied on every open, by name. Locking focus, exposure and white
    /// balance is what makes successive board captures comparable.
    ///
    /// Order matters and is preserved from the file (hence `IndexMap`, not a sorted map):
    /// three Brio controls are `inactive` until a companion auto control is switched off,
    /// so e.g. `focus_automatic_continuous = 0` must appear *before* `focus_absolute`.
    /// The capture loop verifies each write took and warns if not, but correct order is
    /// what makes it take in the first place.
    #[serde(default)]
    pub controls: IndexMap<String, i64>,

    #[serde(default = "default_jpeg_quality")]
    pub mock_jpeg_quality: u8,

    /// Wait this long before re-resolving and reopening after a device failure.
    #[serde(default = "default_reopen_backoff")]
    pub reopen_backoff_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_idle_grace() -> u64 {
    10
}
fn default_broadcast_capacity() -> usize {
    4
}
fn default_snapshot_timeout() -> u64 {
    10
}
fn default_request_timeout() -> u64 {
    5
}
fn default_fourcc() -> String {
    "MJPG".into()
}
fn default_fps() -> u32 {
    30
}
fn default_warmup() -> u32 {
    5
}
fn default_jpeg_quality() -> u8 {
    80
}
fn default_reopen_backoff() -> u64 {
    2
}

/// The only device path prefix that survives a replug.
pub const BY_ID_PREFIX: &str = "/dev/v4l/by-id/";

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in config {}", path.display()))
    }

    /// Parse and validate TOML.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.camera.width == 0 || self.camera.height == 0 {
            bail!("camera.width and camera.height must be non-zero");
        }
        if self.camera.fps == 0 {
            bail!("camera.fps must be non-zero");
        }
        if self.source.broadcast_capacity == 0 {
            bail!("source.broadcast_capacity must be non-zero");
        }
        if self.camera.fourcc.len() != 4 {
            bail!(
                "camera.fourcc must be exactly 4 characters, got {:?}",
                self.camera.fourcc
            );
        }
        if self.source.socket_path.parent().is_none() {
            bail!(
                "source.socket_path {} has no parent directory",
                self.source.socket_path.display()
            );
        }

        if let Some(device) = &self.camera.device
            && !device.starts_with(BY_ID_PREFIX)
        {
            bail!(
                "camera.device must be a {BY_ID_PREFIX}… path, got {}. \
                 The Brio exposes two video nodes and renumbers them on replug, so \
                 /dev/videoN is not a stable reference.",
                device.display()
            );
        }

        if self.camera.backend == Backend::V4l2 && self.camera.device.is_none() {
            bail!(
                "camera.device (a /dev/v4l/by-id/… path) is required when \
                 camera.backend = \"v4l2\""
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_text: &str) -> Result<Config> {
        let config: Config = toml::from_str(toml_text)?;
        config.validate()?;
        Ok(config)
    }

    const MINIMAL: &str = r#"
        [source]
        socket_path = "/tmp/ruru.sock"

        [camera]
        width = 1280
        height = 720
    "#;

    #[test]
    fn minimal_config_gets_sensible_defaults() {
        let c = parse(MINIMAL).unwrap();
        assert_eq!(c.source.idle_grace_secs, 10);
        assert_eq!(c.source.broadcast_capacity, 4);
        assert!(c.source.enabled);
        assert!(!c.source.pin);
        assert!(c.source.socket_group.is_none());
        assert_eq!(c.camera.backend, Backend::Mock);
        assert_eq!(c.camera.fourcc, "MJPG");
        assert_eq!(c.camera.fps, 30);
        assert_eq!(c.camera.warmup_frames, 5);
    }

    #[test]
    fn rejects_dev_video_paths() {
        let toml_text = format!("{MINIMAL}\ndevice = \"/dev/video0\"\n");
        let err = parse(&toml_text).unwrap_err().to_string();
        assert!(err.contains("/dev/v4l/by-id/"), "unexpected error: {err}");
    }

    #[test]
    fn accepts_by_id_paths() {
        let toml_text =
            format!("{MINIMAL}\ndevice = \"/dev/v4l/by-id/usb-046d_MX_Brio_ABC-video-index0\"\n");
        // Still rejected, but for the not-yet-implemented backend rather than the path.
        assert!(parse(&toml_text).is_ok());
    }

    #[test]
    fn v4l2_backend_requires_a_device() {
        // Backend set but no device → rejected.
        let err = parse(&format!("{MINIMAL}\nbackend = \"v4l2\"\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("camera.device"), "unexpected error: {err}");

        // Backend + a valid by-id device → validates.
        let ok = format!(
            "{MINIMAL}\nbackend = \"v4l2\"\ndevice = \"/dev/v4l/by-id/usb-046d_MX_Brio_ABC-video-index0\"\n"
        );
        assert!(parse(&ok).is_ok());
    }

    #[test]
    fn typos_are_rejected_rather_than_silently_ignored() {
        let toml_text = MINIMAL.replace("idle_grace_secs", "idle_grace");
        let toml_text = format!("{toml_text}\nidle_grace = 3\n");
        assert!(parse(&toml_text).is_err());
    }

    #[test]
    fn rejects_zero_dimensions_and_fps() {
        assert!(parse(&MINIMAL.replace("width = 1280", "width = 0")).is_err());
        assert!(parse(&format!("{MINIMAL}\nfps = 0\n")).is_err());
    }

    #[test]
    fn rejects_malformed_fourcc() {
        assert!(parse(&format!("{MINIMAL}\nfourcc = \"MJP\"\n")).is_err());
    }

    #[test]
    fn parses_locked_controls() {
        let toml_text = format!(
            "{MINIMAL}\n[camera.controls]\nfocus_absolute = 40\npower_line_frequency = 1\n"
        );
        let c = parse(&toml_text).unwrap();
        assert_eq!(c.camera.controls.get("focus_absolute"), Some(&40));
        assert_eq!(c.camera.controls.get("power_line_frequency"), Some(&1));
    }

    /// The reason `controls` is an `IndexMap`: a gate must be appliable before the control
    /// it gates. A sorted map would reorder `focus_automatic_continuous` after
    /// `focus_absolute` and silently break the focus lock.
    #[test]
    fn locked_control_order_is_preserved_from_the_file() {
        let toml_text = format!(
            "{MINIMAL}\n[camera.controls]\n\
             focus_automatic_continuous = 0\n\
             focus_absolute = 40\n\
             auto_exposure = 1\n\
             exposure_time_absolute = 250\n"
        );
        let c = parse(&toml_text).unwrap();
        let order: Vec<&str> = c.camera.controls.keys().map(String::as_str).collect();
        assert_eq!(
            order,
            [
                "focus_automatic_continuous",
                "focus_absolute",
                "auto_exposure",
                "exposure_time_absolute",
            ],
            "file order must survive parsing, so gates precede the controls they gate"
        );
    }
}
