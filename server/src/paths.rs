//! Validation of file paths inside a vault.
//!
//! Paths must be storable on both Windows and Android, so the rules follow the
//! stricter of the two (Windows).

use unicode_normalization::UnicodeNormalization;

pub const MAX_PATH_BYTES: usize = 1024;

const FORBIDDEN_CHARS: &[char] = &['<', '>', ':', '"', '|', '?', '*', '\\'];

const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A path in canonical form, plus the lower-cased key used to detect paths
/// that differ only by letter case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPath {
    pub path: String,
    pub key: String,
}

pub fn normalize(raw: &str) -> Result<VaultPath, String> {
    let path: String = raw.nfc().collect();
    if path.is_empty() {
        return Err("path is empty".into());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(format!("path is longer than {MAX_PATH_BYTES} bytes"));
    }
    if path.starts_with('/') {
        return Err("path must be relative".into());
    }
    for segment in path.split('/') {
        check_segment(segment)?;
    }
    let key = path.to_lowercase();
    let top = key.split('/').next().unwrap_or_default();
    if RESERVED_FOLDERS.contains(&top) {
        return Err(format!("files in {top:?} are never synced"));
    }
    Ok(VaultPath { path, key })
}

/// Top-level folders that are never synced, in any letter case. Writing into
/// `.obsidian` could install or change plugins on other devices.
const RESERVED_FOLDERS: &[&str] = &[".obsidian", ".trash", ".git"];

fn check_segment(segment: &str) -> Result<(), String> {
    if segment.is_empty() {
        return Err("path has an empty segment".into());
    }
    if segment == "." || segment == ".." {
        return Err("path may not contain '.' or '..' segments".into());
    }
    if let Some(c) = segment
        .chars()
        .find(|c| c.is_control() || FORBIDDEN_CHARS.contains(c))
    {
        return Err(format!("path contains the forbidden character {c:?}"));
    }
    if segment.ends_with(' ') || segment.ends_with('.') {
        return Err(format!("{segment:?} ends with a space or dot"));
    }
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .to_ascii_lowercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return Err(format!("{segment:?} is a reserved name on Windows"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        for p in [
            "Note.md",
            "Daily/2026-11-06.md",
            "Attachments/photo 1.png",
            ".hidden/file.md",
            "Notes/.obsidian/not-top-level.md",
            ".obsidian-notes/ok.md",
            "Ideas/Café ☕.md",
            "Aux notes.md",
        ] {
            assert!(normalize(p).is_ok(), "{p} should be valid");
        }
    }

    #[test]
    fn rejects_unsafe_paths() {
        for p in [
            "",
            "/abs.md",
            "a//b.md",
            "folder/",
            "../escape.md",
            "a/./b.md",
            "back\\slash.md",
            "what?.md",
            "colon:.md",
            "tab\there.md",
            "trailing.",
            "trailing /x.md",
            "CON",
            "con.md",
            "folder/LPT1.txt",
            ".obsidian/app.json",
            ".Obsidian/plugins/notesync/main.js",
            ".OBSIDIAN",
            ".Trash/old.md",
            ".git/HEAD",
        ] {
            assert!(normalize(p).is_err(), "{p:?} should be rejected");
        }
    }

    #[test]
    fn rejects_overlong_paths() {
        let long = "a".repeat(MAX_PATH_BYTES + 1);
        assert!(normalize(&long).is_err());
        let ok = "a".repeat(MAX_PATH_BYTES);
        assert!(normalize(&ok).is_ok());
    }

    #[test]
    fn normalizes_unicode_to_nfc() {
        // "é" written as "e" plus a combining accent becomes the single character.
        let decomposed = "Cafe\u{0301}.md";
        let result = normalize(decomposed).unwrap();
        assert_eq!(result.path, "Caf\u{00e9}.md");
    }

    #[test]
    fn key_ignores_letter_case() {
        assert_eq!(
            normalize("Notes/A.md").unwrap().key,
            normalize("notes/a.MD").unwrap().key
        );
    }
}
