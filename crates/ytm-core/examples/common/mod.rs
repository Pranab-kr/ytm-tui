//! Cookie-file lookup for the manual spike examples.
//!
//! Reads `~/.config/ytm-tui/config.toml` so the cookie path lives in one file
//! outside the repo and never has to be pasted into a shell command, a
//! transcript, or a process listing.
//!
//! In a subdirectory so Cargo does not treat it as its own example target.

// Compiled separately into every example, and no single one uses all of it.
#![allow(dead_code)]

use std::path::PathBuf;

/// The only auth path. Still an enum so the `let ... else` at each call site
/// keeps reading the same way if another is ever added.
pub enum AuthChoice {
    Cookie(PathBuf),
}

/// Locate the cookie file `auth.cookie_file` names. Errors name the fix rather
/// than the failure: these examples are what you run when nothing else works, so
/// "set this key in this file" is the useful message.
pub fn auth_choice() -> Result<AuthChoice, String> {
    let path = config_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;

    let file = value
        .get("auth")
        .and_then(|a| a.get("cookie_file"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && !s.starts_with("PASTE"))
        .ok_or_else(|| {
            format!(
                "auth.cookie_file is not set in {} — see the README for how to \
                 export your cookies",
                path.display()
            )
        })?;

    let file = PathBuf::from(shellexpand_tilde(file));
    if !file.exists() {
        return Err(format!("cookie file {} does not exist", file.display()));
    }
    Ok(AuthChoice::Cookie(file))
}

/// Duplicates `ytm-tui`'s `config::expand_tilde` on purpose: examples live in
/// `ytm-core`, and importing from the app crate would invert the dependency
/// direction. Not worth a shared crate for nine lines; same for `config_path`.
fn shellexpand_tilde(s: &str) -> String {
    match s.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => s.to_owned(),
        },
        None => s.to_owned(),
    }
}

pub fn config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "ytm-tui")
        .map(|d| d.config_dir().join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}
