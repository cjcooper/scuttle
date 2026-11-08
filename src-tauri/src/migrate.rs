//! One-time migration from the app's previous name, "Container Desktop"
//! (bundle identifier `com.it-problems.container-desktop`), to Scuttle.

use std::path::Path;

pub const OLD_NAME: &str = "Container Desktop";
pub const OLD_IDENTIFIER: &str = "com.it-problems.container-desktop";

/// Files carried over from the old config folder.
const FILES: &[&str] = &["settings.json", "history.json"];

/// Copy settings and history from the old app's config folder (a sibling of
/// `config_dir`, named by the old identifier) when Scuttle has none yet.
/// The old folder is left in place.
pub fn config(config_dir: &Path) {
    if config_dir.join("settings.json").exists() {
        return;
    }
    let Some(old) = config_dir.parent().map(|p| p.join(OLD_IDENTIFIER)) else {
        return;
    };
    if !old.is_dir() {
        return;
    }
    let _ = std::fs::create_dir_all(config_dir);
    for f in FILES {
        let from = old.join(f);
        if from.is_file() {
            let _ = std::fs::copy(&from, config_dir.join(f));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn copies_old_settings_once() {
        let base = std::env::temp_dir().join(format!("scuttle-migrate-{}", std::process::id()));
        let old = base.join(super::OLD_IDENTIFIER);
        let new = base.join("com.it-problems.scuttle");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), "{\"a\":1}").unwrap();
        std::fs::write(old.join("history.json"), "[]").unwrap();

        super::config(&new);
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "{\"a\":1}"
        );
        assert!(new.join("history.json").is_file());

        // Never overwrites Scuttle's own settings.
        std::fs::write(new.join("settings.json"), "{\"b\":2}").unwrap();
        super::config(&new);
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "{\"b\":2}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
