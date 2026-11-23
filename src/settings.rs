//! Editor preferences kept between sessions (`<config>/qucs-s-rs/settings.json`).
//! The theme stays in `theme.txt` (see `theme.rs`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_RECENT: usize = 12;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub color_nets: bool,
    pub pin_names: bool,
    pub show_issues: bool,
    pub show_grid: bool,
    pub auto_save: bool,
    pub auto_reroute: bool,
    /// most recent first
    pub recent: Vec<String>,
    pub left_panel: f32,
    pub right_panel: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            color_nets: false,
            pin_names: false,
            show_issues: true,
            show_grid: true,
            auto_save: false,
            auto_reroute: true,
            recent: vec![],
            left_panel: 230.0,
            right_panel: 290.0,
        }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("qucs-s-rs"))
}

fn path() -> Option<PathBuf> {
    // tests and portable setups can point the settings elsewhere
    if let Some(p) = std::env::var_os("QUCS_RS_SETTINGS") {
        return Some(PathBuf::from(p));
    }
    config_dir().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            // write-then-rename so a crash never leaves half a file
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, t).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }

    /// Put `file` at the top of the recent list.
    pub fn add_recent(&mut self, file: &Path) {
        let s = file.display().to_string();
        self.recent.retain(|r| !same_file(r, &s));
        self.recent.insert(0, s);
        self.recent.truncate(MAX_RECENT);
    }

    /// Recent files that still exist.
    pub fn existing_recent(&self) -> Vec<PathBuf> {
        self.recent.iter().map(PathBuf::from).filter(|p| p.is_file()).collect()
    }
}

fn same_file(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.replace('/', "\\").eq_ignore_ascii_case(&b.replace('/', "\\"))
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_list_is_mru_and_bounded() {
        let mut s = Settings::default();
        for k in 0..20 {
            s.add_recent(Path::new(&format!("C:/x/f{k}.sch")));
        }
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert!(s.recent[0].ends_with("f19.sch"));
        s.add_recent(Path::new("C:/x/f15.sch"));
        assert!(s.recent[0].ends_with("f15.sch"));
        assert_eq!(s.recent.iter().filter(|r| r.ends_with("f15.sch")).count(), 1);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"show_grid": false}"#).unwrap();
        assert!(!s.show_grid);
        assert!(!s.auto_save);
        assert_eq!(s.left_panel, 230.0);
    }

    #[test]
    fn old_window_geometry_is_ignored() {
        let s: Settings = serde_json::from_str(r#"{"window": [-1900.0, 40.0, 800.0, 600.0], "maximized": false, "show_grid": false}"#).unwrap();
        assert!(!s.show_grid);
    }

    #[test]
    fn save_and_load_round_trip() {
        let p = std::env::temp_dir().join(format!("qucs-rs-settings-{}.json", std::process::id()));
        std::env::set_var("QUCS_RS_SETTINGS", &p);
        let mut s = Settings::default();
        s.color_nets = true;
        s.add_recent(Path::new("C:/a.sch"));
        s.save();
        assert_eq!(Settings::load(), s);
        let _ = std::fs::remove_file(&p);
        std::env::remove_var("QUCS_RS_SETTINGS");
    }
}
