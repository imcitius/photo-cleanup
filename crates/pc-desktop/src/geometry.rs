//! Geometry is separate from the data bootstrap: hiding a window must never
//! rewrite the choice of database. Portable state stays beside its data.
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Geometry {
    pub fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn save(self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self)?)?;
        std::fs::rename(tmp, path)
    }

    /// Keep the full rectangle on a connected monitor. If the saved monitor
    /// went away, use the primary work area. Arithmetic is widened because
    /// this JSON is editable and coordinates may be negative or extreme.
    pub fn fit(self, screens: &[Self]) -> Option<Self> {
        let screen = screens
            .iter()
            .find(|s| {
                i64::from(self.x) >= i64::from(s.x)
                    && i64::from(self.y) >= i64::from(s.y)
                    && i64::from(self.x) < i64::from(s.x) + i64::from(s.width)
                    && i64::from(self.y) < i64::from(s.y) + i64::from(s.height)
            })
            .or_else(|| screens.first())?;
        let width = self.width.max(640).min(screen.width);
        let height = self.height.max(480).min(screen.height);
        let x = i64::from(self.x).clamp(
            i64::from(screen.x),
            i64::from(screen.x) + i64::from(screen.width - width),
        );
        let y = i64::from(self.y).clamp(
            i64::from(screen.y),
            i64::from(screen.y) + i64::from(screen.height - height),
        );
        Some(Self {
            x: x as i32,
            y: y as i32,
            width,
            height,
            ..self
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disconnected_monitors_and_corrupt_dimensions_stay_on_screen() {
        let screen = Geometry {
            x: -1440,
            y: 23,
            width: 1440,
            height: 877,
            maximized: false,
        };
        let saved = Geometry {
            x: i32::MAX,
            y: i32::MIN,
            width: u32::MAX,
            height: 0,
            maximized: true,
        };
        let fitted = saved.fit(&[screen]).unwrap();
        assert_eq!(
            fitted,
            Geometry {
                x: -1440,
                y: 23,
                width: 1440,
                height: 480,
                maximized: true
            }
        );
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("window.json");
        fitted.save(&path).unwrap();
        assert_eq!(Geometry::read(&path), Some(fitted));
        assert_eq!(fitted.fit(&[]), None);
    }
}
