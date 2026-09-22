//! What the user has chosen, and where it is kept.
//!
//! Shaped deliberately like `window_position`: one small file in the app's
//! data directory, through the serde_json the app already carries, written
//! when a control changes rather than on quit. PrivacyFlow has no reliable
//! quit hook to write from, because a frameless window has no menu bar for
//! Cmd-Q to reach.
//!
//! Kept separate from `window.json` because where the capsule was left is a
//! remembered fact and this is a preference. One of them being unreadable
//! should not cost the other.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Everything the user has chosen.
///
/// `serde(default)` means a field added by a later version is absent rather
/// than fatal when an older file is read, and unknown fields are ignored, so
/// a file written by a later version still loads here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub minimal_mode: bool,
    /// Whether a short sound confirms the start and end of a dictation.
    pub sound_cues: bool,
    /// The microphone to record from whenever it is connected, by name. None
    /// follows the system input, which is what macOS switches to a headset
    /// the moment one connects.
    pub preferred_microphone: Option<String>,
    /// Whether a dictation's recording is kept after it has been transcribed,
    /// so a different recognizer can be compared against it offline later.
    ///
    /// Off unless the user asks. Audio is the one thing this application has
    /// never kept, and nobody's existing settings file may start keeping it.
    pub retain_audio: bool,
}

/// Written out rather than derived, because `sound_cues` defaults on and a
/// derived `Default` would default it off. `retain_audio` is the mirror of
/// that case and matters more: a derive happens to be correct for it today,
/// and relying on that for a privacy default is relying on an accident. That distinction is not academic:
/// `serde(default)` fills the field for every settings file written before
/// this setting existed, so a derive would silently turn the cues off for
/// everyone who already had a settings.json.
impl Default for Settings {
    fn default() -> Self {
        Self {
            minimal_mode: false,
            sound_cues: true,
            preferred_microphone: None,
            retain_audio: false,
        }
    }
}

/// The settings, and whatever went wrong getting them.
///
/// The two travel together because the caller needs both: it runs on the
/// defaults either way, and it must report the problem rather than let a
/// silently reverted preference look like the user's imagination.
#[derive(Clone)]
pub struct Load {
    pub settings: Settings,
    /// Present only when a file existed and could not be used. A missing file
    /// is a first run, which is not a problem.
    pub problem: Option<String>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// Read the settings, falling back to defaults on anything unreadable and
/// saying so.
///
/// The unreadable file is deliberately left on disk. It is overwritten only
/// when the user changes a setting, because that action is the instruction to
/// replace it; until then it survives for them to look at.
pub fn load(data_dir: &Path) -> Load {
    let path = path(data_dir);
    let failed = |error: String| Load {
        settings: Settings::default(),
        problem: Some(format!("Could not read {}: {error}", path.display())),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Load { settings: Settings::default(), problem: None }
        }
        Err(error) => return failed(error.to_string()),
    };
    match serde_json::from_str(&text) {
        Ok(settings) => Load { settings, problem: None },
        Err(error) => failed(error.to_string()),
    }
}

/// Write the settings, returning what went wrong rather than reporting it.
///
/// Unlike `window_position::save`, which prints and moves on, this has to
/// come back to the caller: there is a control on screen showing the new
/// value, and a write that failed leaves that control describing a state the
/// application is not in.
pub fn save(data_dir: &Path, settings: &Settings) -> Result<(), String> {
    let path = path(data_dir);
    let text = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("Could not encode settings: {error}"))?;
    std::fs::write(&path, text)
        .map_err(|error| format!("Could not write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own directory. The process id keeps concurrent
    /// `cargo test` runs from colliding, and the name says which test left it
    /// behind if one ever fails mid-way.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("privacyflow-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A first run has no file. That is the ordinary case and must not be
    /// reported as a fault, or every new install starts with a red dot.
    #[test]
    fn a_missing_file_is_a_first_run_not_a_problem() {
        let dir = scratch("missing");
        let loaded = load(&dir);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.problem.is_none());
    }

    /// The opposite case, and the one that matters: a file that exists and
    /// cannot be used must not pass silently as a first run, because the user
    /// would see their preference revert with no explanation.
    #[test]
    fn a_malformed_file_gives_defaults_and_says_so() {
        let dir = scratch("malformed");
        std::fs::write(dir.join("settings.json"), "{ this is not json").unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.settings, Settings::default());
        let problem = loaded.problem.expect("a broken settings file must be reported");
        assert!(
            problem.contains("settings.json"),
            "the report must name the file the user has to fix: {problem}"
        );
    }

    /// The exact file every existing install already has on disk. The cues
    /// default on, and `serde(default)` fills the missing field from
    /// `Settings::default`, so this is what catches a derived `Default`
    /// quietly turning them off for everyone who upgraded.
    #[test]
    fn a_file_written_before_the_cues_existed_still_has_them_on() {
        let dir = scratch("pre-cues");
        std::fs::write(dir.join("settings.json"), r#"{"minimal_mode": true}"#).unwrap();
        let loaded = load(&dir);
        assert!(loaded.settings.minimal_mode);
        assert!(loaded.settings.sound_cues, "cues must not default off on upgrade");
        assert_eq!(loaded.settings.preferred_microphone, None, "an upgrade follows the system input");
    }

    /// The appearance work will add fields to this file. A file written by a
    /// later version must still load in an earlier one, and a file written by
    /// an earlier version must not lose the setting it does have.
    #[test]
    fn an_unknown_field_does_not_discard_the_settings_beside_it() {
        let dir = scratch("unknown");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"minimal_mode": true, "theme": "midnight"}"#,
        )
        .unwrap();
        let loaded = load(&dir);
        assert!(loaded.settings.minimal_mode);
        assert!(loaded.problem.is_none(), "an unknown field is not a fault");
    }

    /// The privacy default, protected in the one place it can quietly break.
    /// Every settings file on disk today was written before retention existed,
    /// so `serde(default)` is what decides retention for every existing
    /// install, and it must decide it off.
    #[test]
    fn a_file_written_before_retention_existed_does_not_keep_audio() {
        let dir = scratch("pre-retention");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"minimal_mode": true, "sound_cues": false}"#,
        )
        .unwrap();
        let loaded = load(&dir);
        assert!(
            !loaded.settings.retain_audio,
            "an upgrade must never start keeping recordings on its own"
        );
    }

    #[test]
    fn what_is_saved_is_what_loads_back() {
        let dir = scratch("roundtrip");
        let settings = Settings {
            minimal_mode: true,
            preferred_microphone: Some("MacBook Pro Microphone".to_owned()),
            ..Default::default()
        };
        save(&dir, &settings).unwrap();
        assert_eq!(load(&dir).settings, settings);
    }

    /// A write that cannot happen must come back as a value, not a printed
    /// line nobody sees. A ticked checkbox that did not save is the interface
    /// telling the user something untrue.
    #[test]
    fn a_write_that_cannot_happen_is_returned_not_printed() {
        let dir = scratch("unwritable").join("no-such-directory");
        assert!(save(&dir, &Settings { minimal_mode: true, ..Default::default() }).is_err());
    }
}
