//! Stand-ins for the Mac fonts Scrivener projects name. Linux rarely has
//! Palatino or Baskerville; the editor and PDF Compile draw with the
//! closest installed family instead, while files keep the original names.

use std::collections::HashMap;

/// Picks stand-ins for the Mac fonts Scrivener projects usually name, from
/// what's installed. TeX Gyre's fonts are metric clones of the classics.
pub fn find_substitutes(installed: &[String]) -> HashMap<String, String> {
    let has = |f: &str| installed.iter().any(|i| i.eq_ignore_ascii_case(f));
    let first = |choices: &[&str]| choices.iter().find(|c| has(c)).map(|c| c.to_string());
    let groups: &[(&[&str], &[&str])] = &[
        (
            &["Palatino", "Palatino Linotype", "Book Antiqua"],
            &[
                "TeX Gyre Pagella",
                "URW Palladio L",
                "P052",
                "Liberation Serif",
            ],
        ),
        (
            &["Times", "Times New Roman", "Times Roman"],
            &["TeX Gyre Termes", "Liberation Serif", "Nimbus Roman"],
        ),
        (
            &["Helvetica", "Helvetica Neue", "Arial"],
            &["TeX Gyre Heros", "Liberation Sans", "Nimbus Sans"],
        ),
        (
            &[
                "Courier",
                "Courier New",
                "American Typewriter",
                "Courier Prime",
            ],
            &[
                "Courier Prime",
                "TeX Gyre Cursor",
                "Liberation Mono",
                "Nimbus Mono PS",
            ],
        ),
        (
            &[
                "Baskerville",
                "Big Caslon",
                "Hoefler Text",
                "Georgia",
                "Century Schoolbook",
                "Cochin",
            ],
            &[
                "Libre Baskerville",
                "TeX Gyre Schola",
                "C059",
                "Liberation Serif",
            ],
        ),
    ];
    let mut out = HashMap::new();
    for (mac, linux) in groups {
        if let Some(sub) = first(linux) {
            for name in *mac {
                if !has(name) {
                    out.insert(name.to_string(), sub.clone());
                }
            }
        }
    }
    out
}
