//! Guards the public repo against personal data.
//!
//! Reads `.private-terms` at the repo root (git-ignored, one term per line,
//! `#` starts a comment) and fails if any publishable file contains one of
//! them, case-insensitively. Without that file (e.g. a fresh clone) the test
//! passes: the list itself is personal and never committed.

use std::path::{Path, PathBuf};

/// Directories and root files that end up on GitHub and may carry captures.
const SCANNED_DIRS: &[&str] = &["src", "tests", "docs"];
const SCANNED_FILES: &[&str] = &["README.md", "CLAUDE.md", "Cargo.toml"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_terms(path: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_lowercase)
            .collect(),
    )
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn publishable_files_do_not_contain_private_terms() {
    let root = root();
    let Some(terms) = load_terms(&root.join(".private-terms")) else {
        eprintln!("no .private-terms file; skipping private data check");
        return;
    };
    if terms.is_empty() {
        return;
    }

    let mut files: Vec<PathBuf> = SCANNED_FILES.iter().map(|f| root.join(f)).collect();
    for dir in SCANNED_DIRS {
        collect_files(&root.join(dir), &mut files);
    }

    let mut hits = Vec::new();
    for file in &files {
        // Binary or unreadable files are skipped.
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            let lower = line.to_lowercase();
            for term in &terms {
                if lower.contains(term.as_str()) {
                    let rel = file.strip_prefix(&root).unwrap_or(file);
                    hits.push(format!("{}:{}: {term}", rel.display(), n + 1));
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "private terms found in publishable files:\n{}",
        hits.join("\n")
    );
}
