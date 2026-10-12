//! Counts false losses in a real Scrivener project.
//!
//! The public Windows 3.1.6 manual is the v3 project at
//! <https://www.literatureandlatte.com/docs/scrivener_3-user_manual_source.zip>
//! (Creator `SCRWIN-3.1.6.0`, 2401 `content.rtf` files). Point
//! `OMAQUILL_MANUAL` at the unpacked `.scriv` folder. The test only reads
//! it. With the variable unset, it returns.

use omaquill::rtf;
use std::path::Path;

const REAL_LOSS: &[&str] = &[
    "footnotes",
    "endnotes",
    "comments",
    "images",
    "lists",
    "tables",
    "paragraph styles",
    "bookmarks",
    "headers",
    "footers",
    "index entries",
    "contents entries",
    "inline annotations",
    "revisions",
    "document properties",
    "deeply nested groups",
    "tab intervals",
];

#[test]
fn scrivener_manual_has_no_false_losses() {
    let Some(root) = std::env::var_os("OMAQUILL_MANUAL") else {
        return;
    };
    let root = Path::new(&root);
    let mut files = Vec::new();
    walk(root, &mut files);
    assert!(!files.is_empty(), "no rtf under {}", root.display());

    let mut lossy = 0;
    let mut false_positives = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for path in &files {
        let text = rtf::parse(&std::fs::read(path).unwrap());
        if text.lossy.is_empty() {
            continue;
        }
        lossy += 1;
        let key = text.lossy.join(", ");
        *counts.entry(key).or_default() += 1;
        if text.lossy.iter().all(|name| !REAL_LOSS.contains(name)) {
            false_positives.push(path.display().to_string());
        }
    }
    eprintln!(
        "{} files, {} lossy, {} false positives",
        files.len(),
        lossy,
        false_positives.len()
    );
    for (name, n) in &counts {
        eprintln!("  {n} {name}");
    }
    assert!(
        false_positives.is_empty(),
        "{} scenes with no real loss were flagged: {:?}",
        false_positives.len(),
        &false_positives[..false_positives.len().min(8)]
    );
}

fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rtf") {
            files.push(path);
        }
    }
}
