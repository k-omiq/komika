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

    // 3. Fuzzy blocking when no exact hit: block on the longest title token.
    if candidate_ids.is_empty() {
        if let Some(token) = longest_token(&norm_titles) {
            let ids = catalog::candidate_work_ids_by_token(pool, &token, FUZZY_BLOCK_LIMIT).await?;
            candidate_ids.extend(ids);
        }
    }
    if candidate_ids.is_empty() {
        return Ok(Decision::New);
    }

    // 4. Score every candidate; keep the best.
    let mut best: Option<(f64, String)> = None;
    for wid in &candidate_ids {
        let Some(md) = catalog::load_match_data(pool, wid).await? else {
            continue;
        };
        let score = score_candidate(cand, &norm_titles, &md);
        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, wid.clone()));
        }
    }
    let Some((score, work_id)) = best else {
        return Ok(Decision::New);
    };

    // 5. Decide.
    let method = if exact_hit {
        "title_corroborated"
    } else {
        "fuzzy"
    };
    Ok(if score >= HIGH {
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

/// Confidence in [0,1]: `0.6*title + 0.4*corroboration`, plus small author/year
/// boosters. Title alone (no description/cover overlap) tops out at ~0.6 → Review,
/// so auto-merge needs corroboration on top of the title.
fn score_candidate(cand: &Candidate, norm_titles: &[String], md: &WorkMatchData) -> f64 {
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
    if let Some(p) = phash_similarity(cand.cover_phash.as_deref(), md.cover_phash.as_deref()) {
        corrob = corrob.max(p);
    }

    let mut score = 0.6 * title_sim + 0.4 * corrob;
    if author_matches(&cand.author, &md.author) {
        score += 0.05;
    }
    if year_close(cand.year, md.year) {
        score += 0.03;
    }
    score.min(1.0)
}

fn author_matches(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let na = a.trim().to_lowercase();
            let nb = b.trim().to_lowercase();
            !na.is_empty() && na == nb
        }
        _ => false,
    }
}

fn year_close(a: Option<i64>, b: Option<i64>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if (a - b).abs() <= 1)
}

fn longest_token(norm_titles: &[String]) -> Option<String> {
    norm_titles
        .iter()
        .flat_map(|t| t.split_whitespace())
        .filter(|w| w.chars().count() >= 3)
        .max_by_key(|w| w.chars().count())
        .map(|w| w.to_string())
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
    async fn title_plus_copied_description_auto_merges() {
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
            Decision::AutoMerge { work_id, .. } => assert_eq!(work_id, w),
            other => panic!("expected auto-merge, got {other:?}"),
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
