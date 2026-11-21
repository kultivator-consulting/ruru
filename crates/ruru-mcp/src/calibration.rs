//! The persisted board calibration — corners + measured aspect.
//!
//! A small JSON file `ruru-mcp` owns and rewrites when the admin saves, loaded at startup
//! and overriding the config `[board]` defaults. Keeping it separate means dragging a
//! corner never rewrites the hand-annotated `mcp.toml`.

use crate::config::BoardConfig;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// Four board corners, normalized 0..1 over the raw frame, order TL, TR, BR, BL.
    /// `None` ⇒ uncalibrated; the pipeline falls back to no-warp.
    #[serde(default)]
    pub corners: Option<[[f32; 2]; 4]>,
    /// Measured board aspect (width : height). Warping the wrong aspect stretches text.
    pub aspect: [u32; 2],
}

impl Calibration {
    /// Load the calibration file if present, else seed from the config `[board]` defaults.
    /// A missing file is not an error — it just means "not calibrated yet".
    pub fn load_or_default(path: &Path, board: &BoardConfig) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                corners: board.corners,
                aspect: board.aspect,
            });
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading calibration {}", path.display()))?;
        let cal: Calibration = serde_json::from_str(&text)
            .with_context(|| format!("parsing calibration {}", path.display()))?;
        cal.validate()?;
        Ok(cal)
    }

    pub fn validate(&self) -> Result<()> {
        if self.aspect[0] == 0 || self.aspect[1] == 0 {
            bail!("aspect components must be non-zero");
        }
        if let Some(corners) = &self.corners {
            for (i, [x, y]) in corners.iter().enumerate() {
                if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) {
                    bail!("corner {i} = [{x}, {y}] must be within 0..=1");
                }
            }
        }
        Ok(())
    }

    /// Write atomically (temp file + rename) so a crash mid-write can't corrupt it.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("serializing calibration")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }

    pub fn is_calibrated(&self) -> bool {
        self.corners.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> BoardConfig {
        BoardConfig {
            aspect: [3, 2],
            corners: None,
        }
    }

    #[test]
    fn missing_file_seeds_from_board_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cal = Calibration::load_or_default(&dir.path().join("nope.json"), &board()).unwrap();
        assert_eq!(cal.aspect, [3, 2]);
        assert!(!cal.is_calibrated());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("calibration.json");
        let cal = Calibration {
            corners: Some([[0.1, 0.1], [0.9, 0.12], [0.88, 0.85], [0.12, 0.83]]),
            aspect: [16, 10],
        };
        cal.save(&path).unwrap();
        let loaded = Calibration::load_or_default(&path, &board()).unwrap();
        assert_eq!(loaded, cal);
        assert!(loaded.is_calibrated());
    }

    #[test]
    fn validation_rejects_bad_input() {
        assert!(
            Calibration {
                corners: None,
                aspect: [0, 2]
            }
            .validate()
            .is_err()
        );
        assert!(
            Calibration {
                corners: Some([[0.0, 0.0], [1.5, 0.0], [1.0, 1.0], [0.0, 1.0]]),
                aspect: [3, 2]
            }
            .validate()
            .is_err()
        );
    }
}
