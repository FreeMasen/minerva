//! Identifying a book from its own contents.
//!
//! A book's OPF metadata is often thin or wrong — a Calibre conversion may
//! name its author `HTML to Epub`, or title a file `Book 34 - Thud!` — and
//! fewer than three quarters of a real library carry an ISBN there. But a
//! published book's *text* routinely carries its own identity: the copyright
//! page prints ISBNs, and many books reproduce their Library of Congress
//! Cataloging-in-Publication record verbatim, which is a catalog entry
//! complete with subject headings and a classification number.
//!
//! This module turns that text into [`Evidence`]. It only ever *reports* what
//! the book says; deciding whether to believe it over the stored metadata is
//! left to the caller, because the printed record is sometimes a different
//! edition and the older CIP layout is loose enough to misparse.

use std::sync::LazyLock;

use regex::Regex;

/// What a book's own text says about its identity.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// Checksum-valid ISBNs found in the text, normalized to ISBN-13 and
    /// deduplicated. A book commonly prints several (hardcover, ebook), so
    /// these are candidates rather than one answer.
    pub isbns: Vec<String>,
    /// The Library of Congress Control Number, if the text labels one. This is
    /// LC's own exact key, and resolves against their catalog far more
    /// reliably than an ebook ISBN does.
    pub lccn: Option<String>,
    /// The parsed CIP record, when the book reproduces one.
    pub cip: Option<Cip>,
}

/// A Library of Congress Cataloging-in-Publication record, as printed in the
/// book. Every field is optional: the layout is not a standard either
/// publishers or this parser can rely on.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cip {
    pub style: CipStyle,
    pub title: Option<String>,
    /// The author in the library's inverted form (`Pratchett, Terry`), which
    /// is why it is useful for *confirming* a stored name rather than
    /// replacing it.
    pub author: Option<String>,
    /// Library of Congress Subject Headings.
    pub subjects: Vec<String>,
    /// The Library of Congress call number (`TX765.B466 2014`).
    pub lcc: Option<String>,
    /// The Dewey Decimal number (`641.815`).
    pub ddc: Option<String>,
}

/// Which CIP layout a record was read from. The modern one labels its fields
/// and parses precisely; the older one is positional and needs guarding, so
/// callers may reasonably trust it less.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CipStyle {
    /// `Names: ... | Title: ... | Subjects: LCSH: ...`
    Labelled,
    /// An unlabelled block: inverted author, `title / responsibility`, a
    /// numbered subject list, then the call numbers.
    #[default]
    Classic,
}

// --- ISBN ---

/// The canonical ISBN-13 for a printed ISBN, or `None` if the digits do not
/// checksum. Validation is what makes text scanning safe: without it, any
/// 13-digit string in a book would look like an identifier.
pub fn canonical_isbn(raw: &str) -> Option<String> {
    let digits: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, 'X' | 'x'))
        .collect();
    match digits.len() {
        10 if isbn10_valid(&digits) => Some(isbn10_to_13(&digits)),
        13 if isbn13_valid(&digits) => Some(digits),
        _ => None,
    }
}

fn isbn10_valid(digits: &str) -> bool {
    let mut total = 0u32;
    for (i, ch) in digits.chars().enumerate() {
        let value = match ch {
            'X' | 'x' if i == 9 => 10,
            c if c.is_ascii_digit() => c as u32 - '0' as u32,
            _ => return false,
        };
        total += value * (10 - i as u32);
    }
    total.is_multiple_of(11)
}

fn isbn13_valid(digits: &str) -> bool {
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let total: u32 = digits
        .chars()
        .enumerate()
        .map(|(i, c)| (c as u32 - '0' as u32) * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    total.is_multiple_of(10)
}

fn isbn10_to_13(digits: &str) -> String {
    let body: String = format!("978{}", &digits[..9]);
    let total: u32 = body
        .chars()
        .enumerate()
        .map(|(i, c)| (c as u32 - '0' as u32) * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    let check = (10 - (total % 10)) % 10;
    format!("{body}{check}")
}

// --- Patterns ---

// An ISBN introduced by its own label, the form a copyright page uses.
static LABELLED_ISBN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)ISBN(?:[-\s]*1[03])?\s*[:\s]\s*([0-9][0-9\u{2010}-\u{2015}\- ]{8,18}[0-9Xx])")
        .expect("valid pattern")
});

// A bare ISBN-13, which always begins 978 or 979. Checksum-gated, so a
// coincidental run of digits cannot pass.
static BARE_ISBN13: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(97[89][0-9\u{2010}-\u{2015}\- ]{10,16}[0-9])\b").expect("valid pattern")
});

// Only a labelled LCCN is accepted: bare eight-digit numbers are far too
// common in running text to guess at.
static LCCN_PAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:Library of Congress (?:Control|Catalog(?:ue)?) Number|LCCN)\s*[:\s]\s*([0-9]{8,12})|lccn\.loc\.gov/([0-9]{8,12})",
    )
    .expect("valid pattern")
});

static CIP_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Library of Congress Catalog(?:ing)?[-\s]in[-\s]Publication(?:\s+Data)?")
        .expect("valid pattern")
});

/// An inverted personal name, optionally with dates and a role: the shape the
/// first line of a classic CIP block takes. Deliberately strict — this is the
/// guard that keeps printer's marks and credit lines from being read as names.
static INVERTED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^([A-Z][\p{L}'\u{2019}.\- ]{1,40},\s+[A-Z][\p{L}'\u{2019}.\- ]{1,40}?)(?:,\s*\d{4}[\u{2010}-\u{2015}\-]?\d{0,4})?(?:,?\s*(?:author|editor|illustrator|photographer|translator))?\.?$",
    )
    .expect("valid pattern")
});

// A call number: class letters, a number, an optional decimal extension and
// an optional Cutter number (`TX765.B466`, `QA76.73.P98`).
static LCC_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-Z]{1,3}\s?\d{1,4}(?:\.\d+)?(?:\.[A-Z]\d+)?)").expect("valid pattern")
});

static DDC_PAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9]{3}(?:\.[0-9\u{2019}']+)?)\s*[\u{2010}-\u{2015}\-]*\s*dc\d*")
        .expect("valid pattern")
});

/// Read every identity signal out of a book's text.
pub fn from_text(text: &str) -> Evidence {
    let mut isbns: Vec<String> = Vec::new();
    let mut push = |value: Option<String>| {
        if let Some(value) = value
            && !isbns.contains(&value)
        {
            isbns.push(value);
        }
    };
    for caps in LABELLED_ISBN.captures_iter(text) {
        push(canonical_isbn(&caps[1]));
    }
    for caps in BARE_ISBN13.captures_iter(text) {
        push(canonical_isbn(&caps[1]));
    }

    let lccn = LCCN_PAT.captures(text).and_then(|caps| {
        caps.get(1)
            .or_else(|| caps.get(2))
            .map(|m| m.as_str().to_string())
    });

    // The CIP block is part of `text`, so its own LCCN and ISBNs are already
    // covered by the scans above.
    let cip = parse_cip(text);

    Evidence { isbns, lccn, cip }
}

/// Parse the CIP block out of a book's text, if it has one.
pub fn parse_cip(text: &str) -> Option<Cip> {
    let heading = CIP_HEADING.find(text)?;
    // The record is short; anything far past it is the book's own prose.
    let rest = &text[heading.end()..];
    let block = &rest[..rest.len().min(1200)];

    if block.contains("Names:") || block.contains("Title:") {
        Some(parse_labelled(block))
    } else {
        Some(parse_classic(block))
    }
}

/// Parse the modern layout, which labels every field.
fn parse_labelled(block: &str) -> Cip {
    // Fields run to the next label, so each one's end is the next field's
    // start — not a newline, which publishers place unpredictably.
    const LABELS: [&str; 7] = [
        "Names:",
        "Title:",
        "Description:",
        "Identifiers:",
        "Subjects:",
        "Classification:",
        "LC record",
    ];
    let field = |name: &str| -> Option<String> {
        let start = block.find(name)? + name.len();
        let tail = &block[start..];
        let end = LABELS
            .iter()
            .filter(|label| **label != name)
            .filter_map(|label| tail.find(label))
            .min()
            .unwrap_or(tail.len());
        let value = tail[..end].trim().trim_end_matches('.').trim();
        (!value.is_empty()).then(|| value.to_string())
    };

    let author = field("Names:").map(|names| {
        // "Kim, Eric, author. | Huang, Jenny, photographer." — the first entry
        // is the book's own author; the rest are contributors.
        let first = names.split('|').next().unwrap_or_default().trim();
        strip_name_role(first)
    });
    let title = field("Title:").map(|t| clean_space(&t));
    let subjects = field("Subjects:")
        .map(|s| {
            s.replace("LCSH:", " ")
                .split(['|', ';'])
                .map(|part| clean_space(part.trim().trim_end_matches('.')))
                .filter(|part| !part.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let classification = field("Classification:").unwrap_or_default();
    let lcc = classification
        .find("LCC")
        .map(|i| &classification[i + 3..])
        .and_then(|tail| LCC_CALL.find(tail).map(|m| clean_space(m.as_str())));
    let ddc = classification
        .find("DDC")
        .map(|i| &classification[i + 3..])
        .and_then(|tail| DDC_PAT.captures(tail))
        .map(|caps| caps[1].replace(['\u{2019}', '\''], ""));

    Cip {
        style: CipStyle::Labelled,
        title,
        author: author.filter(|a| !a.is_empty()),
        subjects,
        lcc,
        ddc,
    }
}

/// Parse the older positional layout.
///
/// Every field here is guarded, because this block sits among front-matter
/// text: printer's run marks (`11 12 13 14 15 DIX`), designer credits and
/// omnibus contents all live nearby and will otherwise be read as a title.
fn parse_classic(block: &str) -> Cip {
    let lines: Vec<&str> = block
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    // The author is the inverted name on the record's first line, and only
    // there — later inverted names belong to other works or contributors.
    let author = lines
        .first()
        .and_then(|line| INVERTED_NAME.captures(line))
        .map(|caps| caps[1].trim().to_string());

    // The title line carries a statement of responsibility: "Title / Author".
    let title = lines
        .iter()
        .take(6)
        .filter_map(|line| line.split_once(" / "))
        .map(|(title, _)| clean_space(title.trim().trim_end_matches(':').trim()))
        .find(|title| plausible_title(title));

    let subjects = classic_subjects(block);

    // The call number sits on a line of its own, after the subject list.
    let lcc = lines
        .iter()
        .skip(1)
        .find(|line| {
            LCC_CALL
                .find(line)
                .is_some_and(|m| m.start() == 0 && line.len() <= 40)
                && !line.contains(" / ")
        })
        .and_then(|line| LCC_CALL.find(line).map(|m| clean_space(m.as_str())));

    let ddc = DDC_PAT
        .captures(block)
        .map(|caps| caps[1].replace(['\u{2019}', '\''], ""));

    Cip {
        style: CipStyle::Classic,
        title,
        author,
        subjects,
        lcc,
        ddc,
    }
}

/// The arabic-numbered entries of a classic CIP subject list. Roman-numbered
/// entries are added entries (`I. Title.`), not subjects, and end the list.
fn classic_subjects(block: &str) -> Vec<String> {
    let Some(start) = block.find("1. ") else {
        return Vec::new();
    };
    let tail = &block[start..];
    let mut out = Vec::new();
    let mut current = String::new();
    for token in tail.split_inclusive(' ') {
        let trimmed = token.trim();
        // A roman-numeral entry marker ends the subject list.
        if trimmed.len() <= 5
            && trimmed.ends_with('.')
            && trimmed
                .trim_end_matches('.')
                .chars()
                .all(|c| matches!(c, 'I' | 'V' | 'X'))
            && !trimmed.trim_end_matches('.').is_empty()
        {
            break;
        }
        // An arabic marker starts the next subject.
        let is_marker = trimmed.len() <= 4
            && trimmed.ends_with('.')
            && trimmed.trim_end_matches('.').chars().all(|c| c.is_ascii_digit())
            && !trimmed.trim_end_matches('.').is_empty();
        if is_marker {
            push_subject(&mut out, &current);
            current.clear();
        } else {
            current.push_str(token);
        }
    }
    push_subject(&mut out, &current);
    out
}

fn push_subject(out: &mut Vec<String>, raw: &str) {
    let value = clean_space(&raw.replace('\u{2014}', " -- "))
        .trim_end_matches('.')
        .trim()
        .to_string();
    if !value.is_empty() && value.len() > 2 && !out.contains(&value) {
        out.push(value);
    }
}

/// Whether a candidate title reads like one, rather than like the printer's
/// marks and credits that share the page.
fn plausible_title(title: &str) -> bool {
    if title.len() < 4 || title.len() > 200 {
        return false;
    }
    let letters = title.chars().filter(|c| c.is_alphabetic()).count();
    let digits = title.chars().filter(|c| c.is_ascii_digit()).count();
    // Mostly letters, at least two words, and not a run of numbers.
    letters >= 4
        && letters > digits
        && title.split_whitespace().count() >= 2
        && !title.split_whitespace().all(|w| {
            w.chars()
                .all(|c| c.is_ascii_digit() || c.is_uppercase() || !c.is_alphanumeric())
        })
}

/// Drop a trailing cataloguing role and date from an inverted name.
fn strip_name_role(name: &str) -> String {
    let name = name.trim().trim_end_matches('.').trim();
    let name = INVERTED_NAME
        .captures(name)
        .map(|caps| caps[1].trim().to_string())
        .unwrap_or_else(|| name.to_string());
    clean_space(&name)
}

/// Collapse the whitespace that tag-stripping leaves behind.
fn clean_space(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- Comparing a book's record to what the catalog stores ---

/// Fold a value to its comparable form: accent-free, lowercase, punctuation
/// dropped, whitespace collapsed.
fn fold(value: &str) -> String {
    let ascii = deunicode::deunicode(value).to_lowercase();
    ascii
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a stored title and the one printed in the book's own record name
/// the same work.
///
/// Containment counts either way: a CIP title carries the full subtitle the
/// catalog usually trims (`The Cook's illustrated baking book : baking
/// demystified : 450 recipes...`), so requiring equality would report a
/// disagreement on nearly every book.
pub fn titles_agree(stored: &str, printed: &str) -> bool {
    let (stored, printed) = (fold(stored), fold(printed));
    if stored.is_empty() || printed.is_empty() {
        return false;
    }
    stored == printed || printed.contains(&stored) || stored.contains(&printed)
}

/// Whether a stored author and the one in the book's record name the same
/// person.
///
/// A catalog record inverts names and appends dates and roles
/// (`O'Farrell, Maggie, 1972- author`), so the comparison is on the set of
/// name words rather than their order. Without this, essentially every book
/// would report an author mismatch that is only a difference of form.
pub fn authors_agree(stored: &str, printed: &str) -> bool {
    let words = |value: &str| -> std::collections::BTreeSet<String> {
        fold(value)
            .split_whitespace()
            // Dates and single initials carry no identity.
            .filter(|word| word.len() > 1 && !word.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string)
            .collect()
    };
    let (stored, printed) = (words(stored), words(printed));
    if stored.is_empty() || printed.is_empty() {
        return false;
    }
    stored == printed || printed.is_subset(&stored) || stored.is_subset(&printed)
}

// --- Classification ---

/// Library of Congress classes mapped to browse labels, longest prefix first.
///
/// Unlike a subject string, a call number is a *controlled* classification, so
/// this mapping is exact rather than a guess. Only classes with a clear
/// everyday label are listed; anything else yields nothing, on the same
/// principle as [`crate::subjects`].
const LCC_CATEGORIES: &[(&str, &str)] = &[
    ("QA76", "Programming"),
    ("TX", "Cooking"),
    ("BF", "Psychology"),
    ("BL", "Religion"),
    ("BM", "Religion"),
    ("BP", "Religion"),
    ("BQ", "Religion"),
    ("BR", "Religion"),
    ("BS", "Religion"),
    ("BT", "Religion"),
    ("BV", "Religion"),
    ("BX", "Religion"),
    ("GV", "Games"),
    ("HB", "Business"),
    ("HC", "Business"),
    ("HD", "Business"),
    ("HE", "Business"),
    ("HF", "Business"),
    ("HG", "Business"),
    ("HJ", "Business"),
    ("PA", "Language"),
    ("PE", "Language"),
    ("PC", "Language"),
    ("PL", "Fiction"),
    ("PN", "Fiction"),
    ("PQ", "Fiction"),
    ("PR", "Fiction"),
    ("PS", "Fiction"),
    ("PT", "Fiction"),
    ("PZ", "Children's"),
    ("QA", "Mathematics"),
    ("QB", "Science"),
    ("QC", "Science"),
    ("QD", "Science"),
    ("QE", "Science"),
    ("QH", "Science"),
    ("QK", "Science"),
    ("QL", "Science"),
    ("QP", "Science"),
    ("QR", "Science"),
    ("RA", "Health"),
    ("RC", "Health"),
    ("RM", "Health"),
    ("RT", "Health"),
    ("TK", "Technology"),
    ("TA", "Technology"),
    ("TH", "Technology"),
    ("TL", "Technology"),
    ("TP", "Technology"),
    ("TR", "Art"),
    ("TT", "Art"),
    ("B", "Philosophy"),
    ("D", "History"),
    ("E", "History"),
    ("F", "History"),
    ("G", "Travel"),
    ("H", "Social Science"),
    ("J", "Politics"),
    ("K", "Law"),
    ("L", "Education"),
    ("M", "Music"),
    ("N", "Art"),
    ("Q", "Science"),
    ("R", "Health"),
    ("S", "Agriculture"),
    ("T", "Technology"),
    ("U", "Military"),
    ("V", "Military"),
    ("Z", "Reference"),
];

/// The leading class letters of a call number (`TX765.B466` -> `TX`).
pub fn lcc_class(call_number: &str) -> Option<String> {
    let letters: String = call_number
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    (!letters.is_empty() && letters.len() <= 3).then(|| letters.to_ascii_uppercase())
}

/// The browse category a call number classifies into.
///
/// Matches on the longest listed prefix, so `QA76.73` is `Programming` rather
/// than the `QA` mathematics it sits under.
pub fn lcc_category(call_number: &str) -> Option<&'static str> {
    let call = call_number.trim().to_ascii_uppercase();
    let class = lcc_class(&call)?;
    // Include the first number group, so QA76 can outrank QA.
    let with_number: String = call
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    LCC_CATEGORIES
        .iter()
        .filter(|(prefix, _)| with_number.starts_with(*prefix) || class.starts_with(*prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, label)| *label)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim from a book in a real library, tag-stripped the way
    // `epub::read_text` delivers it.
    const LABELLED_BLOCK: &str = "\
 Library of Congress Cataloging-in-Publication Data \n\
 Names: Kim, Eric, author. | Huang, Jenny, photographer. \n\
 Title: Korean American: food that tastes like home. \n\
 Description: New York: Clarkson Potter/Publishers, 2022. \n\
 Identifiers: LCCN 2021031286 (print) | LCCN 2021031287 (ebook) | ISBN 9780593233498 (hardcover) | ISBN 9780593233504 (ebook) \n\
 Subjects: LCSH: Cooking, Korean. \n\
 Classification: LCC TX724.5.K65 K5428 2022 (print) | LCC TX724.5.K65 (ebook) | DDC 641.59519—dc23 \n\
 LC record: lccn.loc.gov/2021031286 \n";

    const CLASSIC_BLOCK: &str = "\
 Library of Congress Cataloging-in-Publication Data \n\
 Beranbaum, Rose Levy. \n\
 The Baking Bible / Rose Levy Beranbaum. \n\
 pages cm \n\
 ISBN 978-1-118-33861-2 (cloth); 978-0-544-18836-5 (ebook) \n\
 1. Baking. I. Title. \n\
 TX765.B466 2014 \n\
 641.81'5—dc23 \n\
 Print book design by Vertigo Design NYC \n";

    // The front matter that produced every false positive in the prototype.
    const NOISY_BLOCK: &str = "\
 Library of Congress Cataloging-in-Publication Data \n\
 11     12     13     14     15     DIX \n\
 Original interior photography by Evan Sklar \n\
 Cover photography by gephoto \n";

    #[test]
    fn isbn13_is_checksum_validated() {
        assert_eq!(
            canonical_isbn("978-0-593-23350-4").as_deref(),
            Some("9780593233504")
        );
        // A single transposed digit must fail.
        assert_eq!(canonical_isbn("9780593233505"), None);
        assert_eq!(canonical_isbn("1234567890123"), None);
    }

    #[test]
    fn isbn10_is_validated_and_normalized_to_13() {
        // 0-306-40615-2 is a valid ISBN-10; its ISBN-13 is 978-0-306-40615-7.
        assert_eq!(
            canonical_isbn("0-306-40615-2").as_deref(),
            Some("9780306406157")
        );
        // The X check digit is only legal in the final position.
        assert_eq!(canonical_isbn("043942089X").as_deref(), Some("9780439420891"));
        assert_eq!(canonical_isbn("04394X0892"), None);
        assert_eq!(canonical_isbn("0306406153"), None);
    }

    #[test]
    fn finds_every_isbn_a_copyright_page_prints() {
        let evidence = from_text(CLASSIC_BLOCK);
        // Both the cloth and the ebook ISBN, normalized and deduplicated.
        assert_eq!(evidence.isbns, ["9781118338612", "9780544188365"]);
    }

    #[test]
    fn normalizing_collapses_an_isbn_printed_in_both_forms() {
        let text = "ISBN 0-306-40615-2 \n ISBN 978-0-306-40615-7";
        assert_eq!(from_text(text).isbns, ["9780306406157"]);
    }

    #[test]
    fn only_a_labelled_lccn_is_accepted() {
        assert_eq!(from_text(LABELLED_BLOCK).lccn.as_deref(), Some("2021031286"));
        assert_eq!(
            from_text("Library of Congress Control Number: 2014016319")
                .lccn
                .as_deref(),
            Some("2014016319")
        );
        assert_eq!(from_text("lccn.loc.gov/2021031287").lccn.as_deref(), Some("2021031287"));
        // A bare eight-digit number is not an identifier.
        assert_eq!(from_text("printed in 20140163 copies").lccn, None);
    }

    #[test]
    fn parses_the_labelled_cip_layout() {
        let cip = parse_cip(LABELLED_BLOCK).expect("a CIP record");
        assert_eq!(cip.style, CipStyle::Labelled);
        // The first name is the author; the photographer is not.
        assert_eq!(cip.author.as_deref(), Some("Kim, Eric"));
        assert_eq!(
            cip.title.as_deref(),
            Some("Korean American: food that tastes like home")
        );
        assert_eq!(cip.subjects, ["Cooking, Korean"]);
        assert_eq!(cip.lcc.as_deref(), Some("TX724.5.K65"));
        assert_eq!(cip.ddc.as_deref(), Some("641.59519"));
    }

    #[test]
    fn parses_the_classic_cip_layout() {
        let cip = parse_cip(CLASSIC_BLOCK).expect("a CIP record");
        assert_eq!(cip.style, CipStyle::Classic);
        assert_eq!(cip.author.as_deref(), Some("Beranbaum, Rose Levy"));
        assert_eq!(cip.title.as_deref(), Some("The Baking Bible"));
        // "I. Title." is an added entry, not a subject.
        assert_eq!(cip.subjects, ["Baking"]);
        assert_eq!(cip.lcc.as_deref(), Some("TX765.B466"));
        assert_eq!(cip.ddc.as_deref(), Some("641.815"));
    }

    // The prototype read a printer's run mark and a photo credit as titles.
    #[test]
    fn front_matter_noise_yields_no_title_or_author() {
        let cip = parse_cip(NOISY_BLOCK).expect("a block is present");
        assert_eq!(cip.title, None, "a printer's run mark is not a title");
        assert_eq!(cip.author, None, "a credit line is not an author");
    }

    #[test]
    fn a_book_with_no_cip_block_reports_none() {
        assert!(parse_cip("Chapter One. It was a dark and stormy night.").is_none());
        assert_eq!(from_text("nothing identifying here at all"), Evidence::default());
    }

    #[test]
    fn classification_maps_on_the_longest_prefix() {
        // QA76 is computing, not the QA mathematics it sits under.
        assert_eq!(lcc_category("QA76.73.P98 L89 2014"), Some("Programming"));
        assert_eq!(lcc_category("QA303 .S77"), Some("Mathematics"));
        assert_eq!(lcc_category("TX765.B466 2014"), Some("Cooking"));
        assert_eq!(lcc_category("PR6066.R34 H64"), Some("Fiction"));
        assert_eq!(lcc_category("TK5105.888"), Some("Technology"));
        assert_eq!(lcc_category("D743 .B4"), Some("History"));
    }

    #[test]
    fn an_unmapped_or_malformed_class_yields_nothing() {
        assert_eq!(lcc_category(""), None);
        assert_eq!(lcc_category("1955 vol. 14"), None);
        // "CPB Box no. 1955" is a shelf location, not a classification — the
        // shape real LC records sometimes carry in place of a call number.
        assert_eq!(lcc_class("CPB Box no. 1955"), Some("CPB".to_string()));
        assert_eq!(lcc_category("CPB Box no. 1955"), None);
    }

    #[test]
    fn lcc_class_takes_only_the_leading_letters() {
        assert_eq!(lcc_class("TX765.B466"), Some("TX".to_string()));
        assert_eq!(lcc_class("qa76.73"), Some("QA".to_string()));
        assert_eq!(lcc_class("12345"), None);
    }

    #[test]
    fn a_trimmed_title_still_agrees_with_its_full_cip_form() {
        assert!(titles_agree(
            "Cook's Illustrated Baking Book",
            "The Cook\u{2019}s illustrated baking book : baking demystified : 450 recipes"
        ));
        assert!(titles_agree("The Baking Bible", "The Baking Bible"));
    }

    // The mismatch worth surfacing: a Calibre filename artifact.
    #[test]
    fn a_filename_artifact_title_does_not_agree() {
        assert!(!titles_agree("Book 34 - Thud!", "Thud!: a novel of Discworld"));
        assert!(!titles_agree("Peril at End House", "11 12 13 14 15 DIX"));
    }

    #[test]
    fn an_inverted_author_agrees_with_the_display_form() {
        // Every author "disagreement" in a real library was only this.
        for (stored, printed) in [
            ("Rose Levy Beranbaum", "Beranbaum, Rose Levy"),
            ("Margarita Manzke", "Manzke, Margarita, 1974- author"),
            ("Maggie O'Farrell", "O\u{2019}Farrell, Maggie, 1972\u{2013} author"),
            ("Keigo Higashino", "Higashino, Keigo, 1958\u{2013}"),
            ("Terry Pratchett", "Pratchett, Terry"),
        ] {
            assert!(
                authors_agree(stored, printed),
                "{stored:?} should agree with {printed:?}"
            );
        }
    }

    #[test]
    fn a_genuinely_different_author_does_not_agree() {
        assert!(!authors_agree("HTML to Epub", "Pratchett, Terry"));
        assert!(!authors_agree("Terry Pratchett", "Gaiman, Neil"));
        assert!(!authors_agree("", "Pratchett, Terry"));
    }
}
