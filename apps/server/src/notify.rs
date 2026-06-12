//! Inbound notifications — something ANOTHER user did to the recipient's content.
//!
//! Distinct from `user_activity` (the recipient's own OUTBOUND action log). Two kinds:
//!   - `reply`          — someone replied to the recipient's comment (one per reply).
//!   - `like_milestone` — the recipient's comment crossed a like-count milestone.
//!
//! Writes follow the fire-and-forget pattern of `log_activity`: a notification must
//! never fail the mutation that triggered it, so errors are logged and swallowed.

use sqlx::SqlitePool;

/// Like counts that produce a `like_milestone` notification. The first like (1) doubles
/// as the "someone liked your comment" signal; the rest celebrate growth without a
/// notification per like.
pub const LIKE_MILESTONES: &[i64] = &[1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000];

pub fn is_like_milestone(n: i64) -> bool {
    LIKE_MILESTONES.contains(&n)
}

/// Insert one notification for `recipient_id`. `actor_id` is the triggering user (the
/// replier), NULL for an aggregate milestone. `comment_id` is the recipient's own
/// comment the event is about; `target_type`/`target_id` carry its thread for
/// deep-linking; `count` is the milestone value (`like_milestone` only).
#[allow(clippy::too_many_arguments)]
pub async fn record(
    pool: &SqlitePool,
    recipient_id: &str,
    kind: &str,
    actor_id: Option<&str>,
    comment_id: Option<&str>,
    target_type: Option<&str>,
    target_id: Option<&str>,
    count: Option<i64>,
) {
    let res = sqlx::query(
        "INSERT INTO notifications \
           (id, user_id, kind, actor_id, comment_id, target_type, target_id, count, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(recipient_id)
    .bind(kind)
    .bind(actor_id)
    .bind(comment_id)
    .bind(target_type)
    .bind(target_id)
    .bind(count)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, kind, "failed to record notification");
    }
}

/// Whether a `like_milestone` notification for this comment+count already exists, so a
/// like that re-crosses a milestone (after an unlike) doesn't notify twice.
pub async fn like_milestone_exists(pool: &SqlitePool, comment_id: &str, count: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM notifications \
         WHERE comment_id = ? AND kind = 'like_milestone' AND count = ?)",
    )
    .bind(comment_id)
    .bind(count)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
        != 0
}
