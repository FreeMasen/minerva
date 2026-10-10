//! Normalizing EPUB `dc:subject` strings onto a small browsable taxonomy.
//!
//! Subjects as they appear in real EPUBs are a mess: a bare genre
//! (`Fantasy`), a BISAC path (`COMPUTERS / Programming Languages / Python`),
//! several BISAC paths joined with commas, or long-tail noise that is true but
//! useless for browsing (`New York Times bestseller`, `Large type books`).
//!
//! [`normalize`] maps the recognizable ones onto a fixed set of canonical
//! category labels and **drops everything it does not recognize**. Guessing
//! would silently file books under wrong categories, which is worse than
//! leaving them in the broad bucket their folder already gives them.

/// Canonical category labels, keyed by the lowercased subject term that maps
/// to them. Matching is on whole terms, never substrings: `Science Fiction`
/// must not be filed under `Science`, so every multi-word variant that should
/// match is spelled out here.
const SUBJECT_MAP: &[(&str, &str)] = &[
    // --- Broad buckets ---
    ("fiction", "Fiction"),
    ("novel", "Fiction"),
    ("novels", "Fiction"),
    ("nonfiction", "Non-Fiction"),
    ("non fiction", "Non-Fiction"),
    ("non-fiction", "Non-Fiction"),
    // --- Fiction genres ---
    ("fantasy", "Fantasy"),
    ("fantasy fiction", "Fantasy"),
    ("epic fantasy", "Fantasy"),
    ("high fantasy", "Fantasy"),
    ("low fantasy", "Fantasy"),
    ("science fiction", "Science Fiction"),
    ("sci-fi", "Science Fiction"),
    ("science fiction and fantasy", "Science Fiction"),
    ("mystery", "Mystery"),
    ("mystery fiction", "Mystery"),
    ("mystery & detective", "Mystery"),
    ("detective", "Mystery"),
    ("detective and mystery stories", "Mystery"),
    ("crime", "Mystery"),
    ("crime fiction", "Mystery"),
    ("thriller", "Thriller"),
    ("thrillers", "Thriller"),
    ("suspense", "Thriller"),
    ("romance", "Romance"),
    ("horror", "Horror"),
    ("supernatural", "Horror"),
    ("adventure", "Adventure"),
    ("adventure stories", "Adventure"),
    ("action & adventure", "Adventure"),
    ("humor", "Humor"),
    ("humour", "Humor"),
    ("humorous", "Humor"),
    ("humorous fiction", "Humor"),
    ("satire", "Humor"),
    ("comedy", "Humor"),
    ("historical fiction", "Historical Fiction"),
    ("short stories", "Short Stories"),
    ("poetry", "Poetry"),
    ("comics", "Comics"),
    ("graphic novels", "Comics"),
    ("comics & graphic novels", "Comics"),
    ("juvenile fiction", "Children's"),
    ("children's fiction", "Children's"),
    ("young adult", "Young Adult"),
    ("young adult fiction", "Young Adult"),
    // --- Non-fiction subjects ---
    ("biography", "Biography"),
    ("autobiography", "Biography"),
    ("memoir", "Biography"),
    ("biography & autobiography", "Biography"),
    ("history", "History"),
    ("science", "Science"),
    ("physics", "Science"),
    ("chemistry", "Science"),
    ("biology", "Science"),
    ("astronomy", "Science"),
    ("mathematics", "Mathematics"),
    ("computers", "Programming"),
    ("computer science", "Programming"),
    ("programming", "Programming"),
    ("programming languages", "Programming"),
    ("software", "Programming"),
    ("software engineering", "Programming"),
    ("software development", "Programming"),
    ("cooking", "Cooking"),
    ("cookbooks", "Cooking"),
    ("cookery", "Cooking"),
    ("baking", "Cooking"),
    ("home economics", "Cooking"),
    ("business", "Business"),
    ("business & economics", "Business"),
    ("economics", "Business"),
    ("management", "Business"),
    ("self-help", "Self-Help"),
    ("self help", "Self-Help"),
    ("philosophy", "Philosophy"),
    ("religion", "Religion"),
    ("theology", "Religion"),
    ("psychology", "Psychology"),
    ("politics", "Politics"),
    ("political science", "Politics"),
    ("travel", "Travel"),
    ("reference", "Reference"),
    ("education", "Education"),
    ("art", "Art"),
    ("music", "Music"),
    ("technology", "Technology"),
    ("engineering", "Technology"),
    ("health & fitness", "Health"),
    ("medicine", "Health"),
    ("games", "Games"),
    ("games & activities", "Games"),
    ("role playing games", "Games"),
];

/// The canonical category labels a raw `dc:subject` value maps to, in the
/// order they were recognized and without duplicates.
///
/// One value may carry several subjects — BISAC paths are `/`-separated and
/// publishers often comma-join a whole list into one element — so the value is
/// split on both separators and every resulting term is looked up. A term that
/// is not in [`SUBJECT_MAP`] contributes nothing.
pub fn normalize(subject: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for term in subject.split([',', '/', ';']) {
        let term = term.trim().to_lowercase();
        if term.is_empty() {
            continue;
        }
        if let Some((_, label)) = SUBJECT_MAP.iter().find(|(needle, _)| *needle == term)
            && !out.contains(label)
        {
            out.push(label);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_a_bare_genre() {
        assert_eq!(normalize("Fantasy"), vec!["Fantasy"]);
        assert_eq!(normalize("  fantasy  "), vec!["Fantasy"]);
    }

    #[test]
    fn splits_a_bisac_path_and_keeps_every_recognized_level() {
        // The broad bucket and the specific genre are both useful to browse.
        assert_eq!(
            normalize("Fiction / Fantasy / Epic"),
            vec!["Fiction", "Fantasy"]
        );
        assert_eq!(
            normalize("COMPUTERS / Programming Languages / Python"),
            vec!["Programming"]
        );
    }

    #[test]
    fn splits_a_comma_joined_list_of_paths() {
        // Seen verbatim in the library: several BISAC paths in one element.
        assert_eq!(
            normalize("Fiction / Fantasy / Epic, Fiction / Action & Adventure"),
            vec!["Fiction", "Fantasy", "Adventure"]
        );
    }

    // The whole point of matching whole terms rather than substrings.
    #[test]
    fn science_fiction_is_not_science() {
        assert_eq!(normalize("Science Fiction"), vec!["Science Fiction"]);
        assert_eq!(normalize("Science"), vec!["Science"]);
    }

    #[test]
    fn unrecognized_subjects_are_dropped_rather_than_guessed() {
        for noise in [
            "New York Times bestseller",
            "Large type books",
            "Discworld (Imaginary place)",
            "Rincewind the wizard (fictitious character)",
            "General",
            "Man-Woman Relationships",
            "",
        ] {
            assert!(
                normalize(noise).is_empty(),
                "{noise:?} should not have mapped to a category"
            );
        }
    }

    #[test]
    fn duplicates_collapse() {
        assert_eq!(
            normalize("Fiction / Fantasy, fantasy fiction, Fiction"),
            vec!["Fiction", "Fantasy"]
        );
    }
}
