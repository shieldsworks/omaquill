//! Checks against a real Scrivener project that can't live in the repo.
//! Point `OMAQUILL_REAL_PROJECT` at a `.scriv` folder to run them; they
//! only read it.

use omaquill::rtf;
use std::path::PathBuf;

fn project() -> Option<PathBuf> {
    std::env::var_os("OMAQUILL_REAL_PROJECT").map(PathBuf::from)
}

#[test]
fn every_rtf_reads_and_rewrites_stably() {
    let Some(root) = project() else { return };
    let mut n = 0;
    for entry in std::fs::read_dir(root.join("Files/Data")).unwrap() {
        let dir = entry.unwrap().path();
        for name in ["content.rtf", "notes.rtf"] {
            let path = dir.join(name);
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = rtf::parse(&bytes);
            let written = rtf::write(&text);
            let again = rtf::parse(written.as_bytes());
            assert_eq!(again.paragraphs, text.paragraphs, "{}", path.display());
            assert_eq!(rtf::write(&again), written, "{}", path.display());
            if !text.lossy.is_empty() {
                eprintln!("{}: {:?}", path.display(), text.lossy);
            }
            n += 1;
        }
    }
    eprintln!("{n} RTF files round-tripped");
    assert!(n > 0);
}

/// Scrivener's search index holds its own plain-text copy of each
/// document; the text omaquill reads should say the same thing.
#[test]
fn text_matches_scriveners_search_index() {
    let Some(root) = project() else { return };
    let index = std::fs::read_to_string(root.join("Files/search.indexes")).unwrap();
    let index = omaquill::xml::parse(&index).unwrap();
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut checked = 0;
    let mut bad = Vec::new();
    for doc in index.root.child("Documents").unwrap().elements() {
        let id = doc.attr("ID").unwrap();
        for (tag, file) in [("Text", "content.rtf"), ("Notes", "notes.rtf")] {
            let Some(expected) = doc.child_text(tag) else {
                continue;
            };
            // PDFs are indexed too; only RTF is ours to check.
            let Ok(bytes) = std::fs::read(root.join("Files/Data").join(&id).join(file)) else {
                continue;
            };
            let got = rtf::parse(&bytes).plain_text();
            checked += 1;
            if squash(&got) != squash(&expected) {
                bad.push(format!("{id}/{file}"));
                let (g, e) = (squash(&got), squash(&expected));
                let at = g.chars().zip(e.chars()).take_while(|(a, b)| a == b).count();
                let show = |s: &str| {
                    s.chars()
                        .skip(at.saturating_sub(20))
                        .take(60)
                        .collect::<String>()
                };
                eprintln!(
                    "{id}/{file} differs at char {at}:\n  ours:  {:?}\n  index: {:?}",
                    show(&g),
                    show(&e)
                );
            }
        }
    }
    eprintln!("{checked} documents compared");
    assert!(bad.is_empty(), "{bad:?}");
}

/// omaquill's checksums must be the ones Scrivener computes. Scrivener
/// refreshes the list lazily, so a few entries can be stale; the ones that
/// are current must match.
#[test]
fn checksums_match_scriveners() {
    let Some(root) = project() else { return };
    let sums = std::fs::read_to_string(root.join("Files/Data/docs.checksum")).unwrap();
    let (mut n, mut stale) = (0, 0);
    for line in sums.lines() {
        let (k, v) = line.split_once('=').unwrap();
        let (uuid, file) = k.split_once('/').unwrap();
        let Ok(bytes) = std::fs::read(root.join("Files/Data").join(uuid.to_uppercase()).join(file))
        else {
            continue;
        };
        if omaquill::sha1::hex(&bytes) == v {
            n += 1;
        } else {
            stale += 1;
        }
    }
    eprintln!("{n} checksums match, {stale} stale in Scrivener's list");
    assert!(n > stale);
}
