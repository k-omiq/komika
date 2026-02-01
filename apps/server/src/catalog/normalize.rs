//! Title normalization for the dedup matcher's alias index (CATALOGUE.md §4, step 2).
//!
//! Produces a canonical lookup key: lowercased, non-alphanumerics folded to spaces,
//! whitespace collapsed, and a small set of trailing "noise" tokens (season/part
//! markers, format words) stripped. Romanization of raw scripts is intentionally NOT
//! done here — we rely on MangaDex's own romaji alt-titles (`ja-ro`) for that. CJK
//! characters are `char::is_alphanumeric`, so a Japanese title normalizes to its own
//! characters and still matches an identically-normalized MangaDex `ja` alias.

/// Trailing tokens that add no identity and cause near-duplicates to miss
/// (e.g. "Overlord Season 2" vs "Overlord"). Stripped only from the *end*.
const NOISE_TAIL: &[&str] = &[
    "season",
    "part",
    "cour",
    "arc",
    "the",
    "animation",
    "manga",
    "manhwa",
    "manhua",
    "official",
    "colored",
    "color",
    "remake",
    "remaster",
    "remastered",
    "uncensored",
];

/// Normalize a raw title to its alias-index key. Returns `""` for input that has no
/// alphanumeric content (which callers should skip rather than index).
pub fn normalize_title(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let spaced: String = lowered
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut tokens: Vec<&str> = spaced.split_whitespace().collect();

    // Strip trailing noise tokens, plus a trailing bare number that trails a noise
    // word ("season 2" -> both go). Iterate from the end until nothing more strips.
    loop {
        let before = tokens.len();
        if let Some(&last) = tokens.last() {
            let is_number = last.chars().all(|c| c.is_ascii_digit());
            let is_roman = is_roman_numeral(last);
            if NOISE_TAIL.contains(&last) {
                tokens.pop();
            } else if (is_number || is_roman) && tokens.len() >= 2 {
                // Drop a trailing number/roman only when the token before it is noise
                // (e.g. "part iii"); a title ending in a number that carries meaning
                // ("re zero 2" is rare, but "7 seeds" keeps its 7 as a lead token).
                let prev = tokens[tokens.len() - 2];
                if NOISE_TAIL.contains(&prev) {
                    tokens.pop();
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        if tokens.len() == before {
            break;
        }
    }

    tokens.join(" ")
}

fn is_roman_numeral(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| matches!(c, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_folds_punctuation() {
        assert_eq!(
            normalize_title("Re:ZERO -Starting Life-"),
            "re zero starting life"
        );
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(normalize_title("  One    Piece  "), "one piece");
    }

    #[test]
    fn strips_season_and_part_markers() {
        assert_eq!(normalize_title("Overlord Season 2"), "overlord");
        assert_eq!(normalize_title("Berserk Part III"), "berserk");
        assert_eq!(normalize_title("Kingdom (The Animation)"), "kingdom");
    }

    #[test]
    fn keeps_meaningful_leading_numbers() {
        assert_eq!(normalize_title("7 Seeds"), "7 seeds");
        assert_eq!(normalize_title("86 EIGHTY-SIX"), "86 eighty six");
    }

    #[test]
    fn empty_for_symbol_only() {
        assert_eq!(normalize_title("!!!"), "");
    }

    #[test]
    fn cjk_passes_through_lowercased() {
        // CJK chars are alphanumeric, so they survive folding and can match a
        // MangaDex `ja` alias normalized the same way.
        assert_eq!(normalize_title("鬼滅の刃"), "鬼滅の刃");
    }
}
