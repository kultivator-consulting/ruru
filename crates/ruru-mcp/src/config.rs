use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Non-secret configuration. Secrets (key hashes, cookie key) live in a separate `0600`
/// file described by [`SecretsConfig`] — this file can be world-readable.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub source: SourceConfig,
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub board: BoardConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
    #[serde(default)]
    pub calibration: CalibrationConfig,
    #[serde(default)]
    pub captures: CapturesConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to bind. LAN-only plain HTTP — the firewall, not the app, restricts reach.
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,

    /// A slow or hostile client gets this long to send a full request.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,

    /// An admin session older than this must log in again, whatever the browser does.
    #[serde(default = "default_session_max_age")]
    pub session_max_age_secs: u64,

    /// How many image-processing tool calls may run at once; extra calls get a "busy"
    /// tool error rather than queueing.
    #[serde(default = "default_max_concurrent_captures")]
    pub max_concurrent_captures: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            request_timeout_secs: default_request_timeout(),
            session_max_age_secs: default_session_max_age(),
            max_concurrent_captures: default_max_concurrent_captures(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    /// Where `ruru-source` listens. We are a client of it; we never touch the device.
    #[serde(default = "default_source_socket")]
    pub socket_path: PathBuf,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            socket_path: default_source_socket(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    /// Longest-edge ceiling for `capture_whiteboard`. The model downsamples anyway, so
    /// framing matters more than raw pixels; `capture_region` is the full-res escape hatch
    /// and ignores this.
    #[serde(default = "default_max_edge")]
    pub max_edge: u32,

    /// JPEG quality for re-encoded (dewarped/cropped/downscaled) frames. `capture_raw`
    /// passes the source JPEG through untouched and ignores this.
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_edge: default_max_edge(),
            jpeg_quality: default_jpeg_quality(),
        }
    }
}

/// Board geometry seed. Used only when no calibration file exists yet; after that the
/// admin panel's calibration (corners + aspect) in `calibration.json` is authoritative.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardConfig {
    /// Physical aspect ratio (width : height) of the dewarped board.
    #[serde(default = "default_aspect")]
    pub aspect: [u32; 2],

    /// Four board corners in normalized source coordinates (0..1), clockwise from
    /// top-left. `None` until calibrated.
    #[serde(default)]
    pub corners: Option<[[f32; 2]; 4]>,
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            aspect: default_aspect(),
            corners: None,
        }
    }
}

impl BoardConfig {
    pub fn is_calibrated(&self) -> bool {
        self.corners.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    /// Path to the `0600` secrets file (bearer hash, password hash, cookie key).
    #[serde(default = "default_secrets_path")]
    pub path: PathBuf,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            path: default_secrets_path(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationConfig {
    /// Path to the calibration file (corners + aspect), which ruru-mcp owns and rewrites
    /// when the admin saves. World-readable; holds no secrets.
    #[serde(default = "default_calibration_path")]
    pub path: PathBuf,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            path: default_calibration_path(),
        }
    }
}

/// Retained captures: each user-facing snapshot is written to `dir` under an
/// unguessable id and served at `<public_base_url>/captures/<id>.jpg`, so the agent can hand
/// the user a link to the exact frame. **Off** unless both `dir` and `public_base_url` are
/// set — a link needs an on-disk file and an origin to be worth anything.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturesConfig {
    /// Directory the JPEGs are written to (a writable state dir; `/etc` is read-only to the
    /// hardened unit). Unset ⇒ retention off.
    #[serde(default)]
    pub dir: Option<PathBuf>,

    /// Absolute origin used to build view links, e.g. `"http://gothmog:8080"`. The server
    /// can't know its own externally-reachable origin, so it must be configured. Unset ⇒ off.
    #[serde(default)]
    pub public_base_url: Option<String>,

    /// Prune captures older than this many days.
    #[serde(default = "default_retain_days")]
    pub retain_days: u64,

    /// Also cap the number of retained captures (oldest pruned first), so a busy day can't
    /// fill the disk before the age sweep catches up.
    #[serde(default = "default_retain_count")]
    pub retain_count: usize,
}

impl Default for CapturesConfig {
    fn default() -> Self {
        Self {
            dir: None,
            public_base_url: None,
            retain_days: default_retain_days(),
            retain_count: default_retain_count(),
        }
    }
}

fn default_retain_days() -> u64 {
    7
}
fn default_retain_count() -> usize {
    200
}

fn default_bind() -> SocketAddr {
    // 0.0.0.0 so LAN agents can reach it; the firewall scopes who actually can.
    "0.0.0.0:8080".parse().expect("valid default bind")
}
fn default_request_timeout() -> u64 {
    30
}
fn default_session_max_age() -> u64 {
    12 * 60 * 60
}
fn default_max_concurrent_captures() -> usize {
    2
}
fn default_source_socket() -> PathBuf {
    PathBuf::from("/run/ruru/source.sock")
}
fn default_secrets_path() -> PathBuf {
    PathBuf::from("/etc/ruru/mcp-secrets.toml")
}
fn default_calibration_path() -> PathBuf {
    PathBuf::from("/etc/ruru/calibration.json")
}
fn default_max_edge() -> u32 {
    1568
}
fn default_jpeg_quality() -> u8 {
    85
}
fn default_aspect() -> [u32; 2] {
    [3, 2]
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in config {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.pipeline.max_edge < 64 {
            bail!(
                "pipeline.max_edge = {} is implausibly small",
                self.pipeline.max_edge
            );
        }
        if !(1..=100).contains(&self.pipeline.jpeg_quality) {
            bail!(
                "pipeline.jpeg_quality must be 1..=100, got {}",
                self.pipeline.jpeg_quality
            );
        }
        if self.board.aspect[0] == 0 || self.board.aspect[1] == 0 {
            bail!("board.aspect components must be non-zero");
        }
        if let Some(corners) = &self.board.corners {
            for (i, [x, y]) in corners.iter().enumerate() {
                if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) {
                    bail!("board.corners[{i}] = [{x}, {y}] must be within 0..=1");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [server]
        [source]
        [pipeline]
    "#;

    #[test]
    fn minimal_config_defaults() {
        let c = Config::parse(MINIMAL).unwrap();
        assert_eq!(c.server.bind.port(), 8080);
        assert_eq!(c.pipeline.max_edge, 1568);
        assert_eq!(c.pipeline.jpeg_quality, 85);
        assert_eq!(c.board.aspect, [3, 2]);
        assert!(!c.board.is_calibrated());
        assert_eq!(c.secrets.path, PathBuf::from("/etc/ruru/mcp-secrets.toml"));
    }

    #[test]
    fn rejects_bad_quality_and_edge() {
        assert!(Config::parse(&format!("{MINIMAL}\njpeg_quality = 0\n")).is_err());
        let bad = "[server]\n[source]\n[pipeline]\nmax_edge = 8\n";
        assert!(Config::parse(bad).is_err());
    }

    #[test]
    fn rejects_out_of_range_corners() {
        let toml = r#"
            [server]
            [source]
            [pipeline]
            [board]
            corners = [[0.0,0.0],[1.5,0.0],[1.0,1.0],[0.0,1.0]]
        "#;
        assert!(Config::parse(toml).is_err());
    }

    #[test]
    fn unknown_keys_rejected() {
        assert!(Config::parse("[server]\nnope = 1\n[source]\n[pipeline]\n").is_err());
    }

    #[test]
    fn captures_off_by_default_on_when_configured() {
        let c = Config::parse(MINIMAL).unwrap();
        assert!(c.captures.dir.is_none(), "retention off unless configured");
        assert_eq!(c.captures.retain_days, 7);
        assert_eq!(c.captures.retain_count, 200);

        let on = format!(
            "{MINIMAL}\n[captures]\ndir = \"/var/lib/ruru/captures\"\n\
             public_base_url = \"http://gothmog:8080\"\nretain_days = 3\n"
        );
        let c = Config::parse(&on).unwrap();
        assert_eq!(
            c.captures.dir.as_deref(),
            Some(Path::new("/var/lib/ruru/captures"))
        );
        assert_eq!(
            c.captures.public_base_url.as_deref(),
            Some("http://gothmog:8080")
        );
        assert_eq!(c.captures.retain_days, 3);
        assert_eq!(
            c.captures.retain_count, 200,
            "unset fields keep their defaults"
        );
    }
}
