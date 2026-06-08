//! The dedup matcher (CATALOGUE.md §4).
//!
//! Resolves a source-series to a canonical `work` via a precision ladder:
//!   1. external-ID exact          -> auto-merge (highest precision, stop on hit)
//!   2. normalized-title exact      -> candidate
//!   3. fuzzy title (token block)   -> candidate shortlist
//!   4. corroborate (description / cover pHash / author / year) -> confidence score
//!   5. decide: high -> auto-merge, mid -> manual review queue, low -> new work
//!
//! Runs at add-time for Tier-2 series, where a human reviews the mid band — so the
//! decision favors caution: title-only matches land in `Review`, and only an
//! external-ID hit or a title match corroborated by description/cover reaches
//! `AutoMerge`.

use std::collections::HashSet;

use anyhow::Result;
use sqlx::SqlitePool;

use crate::catalog::{
    self,
    normalize::normalize_title,
    similarity::{description_similarity, phash_similarity, title_similarity},
    WorkMatchData,
};

/// A source-series to resolve against the canonical spine.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub title: String,
    pub alt_titles: Vec<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub year: Option<i64>,
    pub cover_phash: Option<String>,
    /// `(provider, external_id)` the source exposes (AniList/MAL/…), if any.
    pub external_ids: Vec<(String, String)>,
}

/// The matcher's verdict for a candidate.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    AutoMerge {
        work_id: String,
        score: f64,
        method: String,
    },
    Review {
        work_id: String,
        score: f64,
        method: String,
    },
    New,
}

/// >= HIGH auto-merges; >= MID goes to the manual review queue; below is a new work.
pub const HIGH: f64 = 0.85;
pub const MID: f64 = 0.6;
const FUZZY_BLOCK_LIMIT: i64 = 50;
/// How many of the longest title tokens the fuzzy block keys on (DD4). Keying on
/// only the single longest token missed a merge whenever the discriminating token
/// wasn't the longest; unioning the top-N candidate sets closes that recall gap.
const FUZZY_BLOCK_TOKENS: usize = 3;
/// Minimum cover-pHash similarity that counts as corroboration for a title-driven
/// auto-merge (DD3). A shared normalized title plus mere description overlap must
/// not auto-merge on its own — without a corroborating cover it lands in Review.
/// Set to 0.90 on the 64-bit dHash → at most ~6 differing bits, tight enough that
/// two genuinely different covers don't corroborate a merge (was 0.8 = up to 12
/// bits, which let flat/letterboxed covers false-merge distinct series).
const PHASH_CORROBORATION: f64 = 0.90;
/// A near-uniform dHash (almost-all-0 or almost-all-1 bits) carries little signal —
/// flat/letterboxed/dark covers collapse to such hashes, so two DIFFERENT series
/// can share one. Only accept a cover as corroborating when its own hash's popcount
/// sits away from both extremes (12..=52 of 64 bits).
fn is_discriminative_phash(hex: &str) -> bool {
    let Ok(bytes) = hex::decode(hex) else {
        return false;
    };
    if bytes.len() != 8 {
        return false;
    }
    let ones: u32 = bytes.iter().map(|b| b.count_ones()).sum();
    (12..=52).contains(&ones)
}

/// Resolve `cand` against the canonical works in `pool`.
pub async fn resolve(pool: &SqlitePool, cand: &Candidate) -> Result<Decision> {
    // 1. External-ID exact — highest precision, stop on hit.
    for (provider, ext) in &cand.external_ids {
        if provider.is_empty() || ext.is_empty() {
            continue;
        }
        if let Some(work_id) = catalog::find_work_by_external(pool, provider, ext).await? {
            return Ok(Decision::AutoMerge {
                work_id,
                score: 1.0,
                method: "external_id".into(),
            });
        }
    }

    // Normalized title set (primary + alts).
    let mut norm_titles: Vec<String> = std::iter::once(cand.title.clone())
        .chain(cand.alt_titles.iter().cloned())
        .map(|t| normalize_title(&t))
        .filter(|s| !s.is_empty())
        .collect();
    norm_titles.sort();
    norm_titles.dedup();
    if norm_titles.is_empty() {
        return Ok(Decision::New);
    }

    // 2. Normalized-title exact -> candidate ids.
    let mut candidate_ids: HashSet<String> = HashSet::new();
    let mut exact_hit = false;
    for nt in &norm_titles {
        let ids = catalog::find_works_by_alias(pool, nt).await?;
        if !ids.is_empty() {
            exact_hit = true;
        }
        candidate_ids.extend(ids);
    }

    // 3. Fuzzy blocking when no exact hit: block on the top-N longest title tokens
    //    (union), so the discriminating token being shorter than a generic one
    //    doesn't drop the real match (DD4).
    if candidate_ids.is_empty() {
        for token in top_tokens(&norm_titles, FUZZY_BLOCK_TOKENS) {
            let ids = catalog::candidate_work_ids_by_token(pool, &token, FUZZY_BLOCK_LIMIT).await?;
            candidate_ids.extend(ids);
        }
    }
    if candidate_ids.is_empty() {
        return Ok(Decision::New);
    }

    // 4. Score every candidate; keep the best (carrying whether the cover pHash
    //    corroborated it, needed for the DD3 auto-merge guard). `candidate_ids` is a
    //    HashSet with nondeterministic iteration, so ties are broken by the lowest
    //    work_id — otherwise the surfaced Review candidate would be arbitrary across
    //    runs when several equal-scoring works share the exact title (DD7).
    let mut best: Option<(Scored, String)> = None;
    for wid in &candidate_ids {
        let Some(md) = catalog::load_match_data(pool, wid).await? else {
            continue;
        };
        let scored = score_candidate(cand, &norm_titles, &md);
        let replace = match best.as_ref() {
            None => true,
            Some((s, w)) => scored.score > s.score || (scored.score == s.score && wid < w),
        };
        if replace {
            best = Some((scored, wid.clone()));
        }
    }
    let Some((scored, work_id)) = best else {
        return Ok(Decision::New);
    };
    let score = scored.score;

    // 5. Decide. A title-driven match (the only kind that reaches here — external-ID
    //    hits returned above) auto-merges only when a corroborating cover backs it, so
    //    a shared common title plus description overlap alone routes to Review (DD3).
    // `method` labels align to the documented `merge_candidate.method` enum
    // (migration 0005): external_id / title_exact / fuzzy / description / cover. The
    // scored path emits title_exact (exact alias hit) or fuzzy (token-block hit); the
    // finer description/cover distinction isn't surfaced separately (DD6).
    let method = if exact_hit { "title_exact" } else { "fuzzy" };
    // Require both a tight pHash match AND that the candidate's own cover hash is
    // discriminative — a near-uniform hash must not corroborate an auto-merge.
    let cover_corroborated = scored
        .phash_sim
        .map(|p| p >= PHASH_CORROBORATION)
        .unwrap_or(false)
        && cand
            .cover_phash
            .as_deref()
            .map(is_discriminative_phash)
            .unwrap_or(false);
    Ok(if score >= HIGH && cover_corroborated {
        Decision::AutoMerge {
            work_id,
            score,
            method: method.into(),
        }
    } else if score >= MID {
        Decision::Review {
            work_id,
            score,
            method: method.into(),
        }
    } else {
        Decision::New
    })
}

/// A candidate's confidence plus the cover-pHash signal that fed it (kept separate so
/// the decision can require cover corroboration for a title-driven auto-merge — DD3).
struct Scored {
    score: f64,
    /// Cover-pHash similarity vs the candidate work, if both had a hash.
    phash_sim: Option<f64>,
}

/// Confidence in [0,1]: `0.6*title + 0.4*corroboration`, plus small author/year
/// boosters. Title alone (no description/cover overlap) tops out at ~0.6 → Review,
/// so auto-merge needs corroboration on top of the title.
fn score_candidate(cand: &Candidate, norm_titles: &[String], md: &WorkMatchData) -> Scored {
    let title_sim = norm_titles
        .iter()
        .flat_map(|nt| {
            md.aliases_norm
                .iter()
                .map(move |al| title_similarity(nt, al))
        })
        .fold(0.0_f64, f64::max);

    let mut corrob = 0.0_f64;
    if let (Some(a), Some(b)) = (&cand.description, &md.description) {
        corrob = corrob.max(description_similarity(a, b));
    }
    let phash_sim = phash_similarity(cand.cover_phash.as_deref(), md.cover_phash.as_deref());
    if let Some(p) = phash_sim {
        corrob = corrob.max(p);
    }

    let mut score = 0.6 * title_sim + 0.4 * corrob;
    if author_matches(&cand.author, &md.author) {
        score += 0.05;
    }
    if year_close(cand.year, md.year) {
        score += 0.03;
    }
    Scored {
        score: score.min(1.0),
        phash_sim,
    }
}

/// Order-insensitive author-name key: lowercased word tokens, sorted (D1). Folds
/// "Masashi Kishimoto" and "Kishimoto Masashi" to the same key so a cross-source
/// name-order variance doesn't defeat author corroboration.
fn author_key(name: &str) -> Vec<String> {
    let mut toks: Vec<String> = name
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    toks.sort();
    toks
}

fn author_matches(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let ka = author_key(a);
            // Non-empty and equal as a sorted token multiset (order-insensitive).
            !ka.is_empty() && ka == author_key(b)
        }
        _ => false,
    }
}

fn year_close(a: Option<i64>, b: Option<i64>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if (a - b).abs() <= 1)
}

/// The `n` longest distinct title tokens (≥3 chars), longest first — the fuzzy block
/// keys on these (DD4). Ties broken lexically so the selection is deterministic.
fn top_tokens(norm_titles: &[String], n: usize) -> Vec<String> {
    let mut toks: Vec<String> = norm_titles
        .iter()
        .flat_map(|t| t.split_whitespace())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_string())
        .collect();
    // Longest first, then lexical — so equal-length duplicates sit adjacent for dedup.
    toks.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()).then(a.cmp(b)));
    toks.dedup();
    toks.truncate(n);
    toks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Alias, WorkInput};

    async fn pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn seed_slime(pool: &SqlitePool) -> String {
        let input = WorkInput {
            primary_title: Some("That Time I Got Reincarnated as a Slime".into()),
            description: Some(
                "The ordinary Mikami Satoru found himself dying after being stabbed by a slasher."
                    .into(),
            ),
            year: Some(2015),
            author: Some("Fuse".into()),
            cover_phash: Some("ffff0000ffff0000".into()),
            aliases: vec![
                Alias {
                    raw: "That Time I Got Reincarnated as a Slime".into(),
                    lang: Some("en".into()),
                },
                Alias {
                    raw: "Tensei Shitara Slime Datta Ken".into(),
                    lang: Some("ja-ro".into()),
                },
            ],
            external_ids: vec![("al".into(), "101517".into())],
            ..Default::default()
        };
        catalog::upsert_work_from_mangadex(pool, "md-slime", &input)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn external_id_auto_merges() {
        let pool = pool().await;
        let w = seed_slime(&pool).await;
        let cand = Candidate {
            title: "Totally Different Title".into(),
            external_ids: vec![("al".into(), "101517".into())],
            ..Default::default()
        };
        match resolve(&pool, &cand).await.unwrap() {
            Decision::AutoMerge {
                work_id,
                method,
                score,
            } => {
                assert_eq!(work_id, w);
                assert_eq!(method, "external_id");
                assert_eq!(score, 1.0);
            }
            other => panic!("expected external-id auto-merge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn title_only_goes_to_review() {
        let pool = pool().await;
        let w = seed_slime(&pool).await;
        // Same alt-title, but no description/cover to corroborate.
        let cand = Candidate {
            title: "Tensei Shitara Slime Datta Ken".into(),
            ..Default::default()
        };
        match resolve(&pool, &cand).await.unwrap() {
            Decision::Review { work_id, .. } => assert_eq!(work_id, w),
            other => panic!("expected review, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exact_title_with_description_only_goes_to_review() {
        // DD3: an exact alt-title match whose only corroboration is an overlapping
        // description (no cover pHash) must NOT auto-merge — a same-titled but distinct
        // work with a copied blurb would otherwise merge unseen. It lands in Review.
        let pool = pool().await;
        let w = seed_slime(&pool).await;
        let cand = Candidate {
            title: "Tensei Shitara Slime Datta Ken".into(),
            description: Some(
                "The ordinary Mikami Satoru found himself dying after being stabbed by a slasher."
                    .into(),
            ),
            ..Default::default()
        };
        match resolve(&pool, &cand).await.unwrap() {
            Decision::Review { work_id, .. } => assert_eq!(work_id, w),
            other => panic!("expected review (DD3: no cover corroboration), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exact_title_with_cover_phash_auto_merges() {
        // DD3: the same exact-title match auto-merges once a corroborating cover backs
        // it — here an identical pHash to the seeded work's `ffff0000ffff0000`.
        let pool = pool().await;
        let w = seed_slime(&pool).await;
        let cand = Candidate {
            title: "Tensei Shitara Slime Datta Ken".into(),
            cover_phash: Some("ffff0000ffff0000".into()),
            ..Default::default()
        };
        match resolve(&pool, &cand).await.unwrap() {
            Decision::AutoMerge { work_id, .. } => assert_eq!(work_id, w),
            other => panic!("expected auto-merge (cover corroborated), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fuzzy_block_finds_work_when_discriminating_token_is_not_longest() {
        // DD4: the fuzzy block used to key on only the single longest token. The
        // existing work's alias is "phantom dungeon"; the candidate adds a strictly
        // longer, non-shared word ("extraordinary", 13 > 7) so the single-longest block
        // keys on a token absent from every alias → returned nothing → Decision::New (a
        // silent duplicate). The top-N union still keys on the shared "dungeon"/"phantom"
        // tokens and finds it. A copied description lifts it into the Review band.
        let pool = pool().await;
        let blurb = "A lone delver descends the phantom dungeon beneath the ruined capital.";
        let existing = catalog::create_work(
            &pool,
            &WorkInput {
                primary_title: Some("Phantom Dungeon".into()),
                description: Some(blurb.into()),
                aliases: vec![Alias {
                    raw: "Phantom Dungeon".into(),
                    lang: None,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let cand = Candidate {
            title: "Extraordinary Phantom Dungeon".into(),
            description: Some(blurb.into()),
            ..Default::default()
        };
        match resolve(&pool, &cand).await.unwrap() {
            Decision::New => panic!("DD4: multi-token block should have found the work"),
            Decision::Review { work_id, .. } | Decision::AutoMerge { work_id, .. } => {
                assert_eq!(work_id, existing)
            }
        }
    }

    #[tokio::test]
    async fn tied_review_candidate_is_deterministic() {
        // DD7: two distinct works share the exact normalized title with no
        // corroboration → equal score → Review. The candidate set is a HashSet
        // (nondeterministic order), so the surfaced work_id must be pinned by the
        // lowest-work_id tiebreak and stable across repeated resolves.
        let pool = pool().await;
        let mut ids = Vec::new();
        for _ in 0..2 {
            ids.push(
                catalog::create_work(
                    &pool,
                    &WorkInput {
                        primary_title: Some("Ambiguous Title".into()),
                        aliases: vec![Alias {
                            raw: "Ambiguous Title".into(),
                            lang: None,
                        }],
                        ..Default::default()
                    },
                )
                .await
                .unwrap(),
            );
        }
        let expected = ids.iter().min().unwrap().clone();
        let cand = Candidate {
            title: "Ambiguous Title".into(),
            ..Default::default()
        };
        for _ in 0..3 {
            match resolve(&pool, &cand).await.unwrap() {
                Decision::Review { work_id, .. } => assert_eq!(work_id, expected),
                other => panic!("expected review, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn matches_via_localized_alt_title_across_extensions() {
        // S2: the cross-extension dedup payoff. A MangaDex spine entry stores ALL
        // its localized alt titles (here the romaji `ja-ro`). A series arriving
        // from another extension under only that localized title — with no shared
        // description or cover — must still resolve to the same work (via Review,
        // since title-only lacks corroboration). This is what consolidates the
        // same series across extensions that each label it differently.
        let pool = pool().await;
        let w = seed_slime(&pool).await; // primary "That Time..."; alt "Tensei Shitara Slime Datta Ken"
        let localized = Candidate {
            // A different extension only knows the romaji title.
            title: "Tensei Shitara Slime Datta Ken".into(),
            ..Default::default()
        };
        match resolve(&pool, &localized).await.unwrap() {
            Decision::Review {
                work_id, method, ..
            } => {
                assert_eq!(work_id, w, "localized title resolves to the spine work");
                assert_eq!(
                    method, "title_exact",
                    "matched via the exact alt-title alias"
                );
            }
            other => panic!("expected review via alt title, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn author_corroboration_is_order_insensitive_lifts_above_mid() {
        // D1: a federated hit that IS the existing MangaDex-anchored work — same
        // title, same author but with the name order reversed (the real Naruto
        // case: "Kishimoto Masashi" vs "Masashi Kishimoto") — must corroborate
        // above the bare-title MID band so it can consolidate, NOT mint a dup.
        let pool = pool().await;
        let existing = catalog::create_work(
            &pool,
            &WorkInput {
                primary_title: Some("Naruto".into()),
                author: Some("Kishimoto Masashi".into()),
                aliases: vec![Alias {
                    raw: "Naruto".into(),
                    lang: None,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Reversed author order + a genuinely different (source-specific) blurb.
        let cand = Candidate {
            title: "Naruto".into(),
            author: Some("Masashi Kishimoto".into()),
            description: Some("A ninja's tale from a different source.".into()),
            ..Default::default()
        };
        match resolve(&pool, &cand).await.unwrap() {
            Decision::Review { work_id, score, .. } => {
                assert_eq!(work_id, existing);
                assert!(
                    score > MID,
                    "author corroboration must lift the score above MID ({MID}); got {score}"
                );
            }
            other => panic!("expected a review-band match above MID, got {other:?}"),
        }

        // But a bare title-only hit with NO author still scores exactly MID (the
        // C2 guard's floor) — corroboration genuinely absent, no false lift.
        let bare = Candidate {
            title: "Naruto".into(),
            ..Default::default()
        };
        match resolve(&pool, &bare).await.unwrap() {
            Decision::Review { score, .. } => {
                assert!(
                    (score - MID).abs() < 1e-9,
                    "bare title-only must stay at MID; got {score}"
                );
            }
            other => panic!("expected bare-title review at MID, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unrelated_title_is_new() {
        let pool = pool().await;
        seed_slime(&pool).await;
        let cand = Candidate {
            title: "Berserk".into(),
            ..Default::default()
        };
        assert_eq!(resolve(&pool, &cand).await.unwrap(), Decision::New);
    }
}
