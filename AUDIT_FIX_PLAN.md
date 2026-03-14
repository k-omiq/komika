# Komika Audit Remediation — Phased Fix Plan

> Companion to [AUDIT_FINDINGS.md](AUDIT_FINDINGS.md). That document is the evidence base
> (every finding has an ID like `A1`, `M3`, `DD2`, `N5`, quoted `file:line`, a failure
> scenario, and a fix direction). **This document is the execution plan** — it sequences
> the fixes into phases, groups the ones that must change together, and tracks progress so
> work can continue across sessions.
>
> Read AUDIT_FINDINGS.md for the full evidence on any ID referenced below before touching it.

## How to use this plan (per session)

1. **Pick the next unchecked item** in the earliest incomplete phase (top-down). Do **one item
   at a time** unless the item explicitly says "do together with …".
2. **Branch** if not already on one: `git checkout -b audit-fixes/<phaseN>-<slug>` (never work on `main`).
3. **Re-read the finding** in AUDIT_FINDINGS.md (the `file:line` evidence may have shifted — verify
   against the current code, don't trust line numbers blindly).
4. **Implement the fix**, matching surrounding code style.
5. **Verify** (see per-phase notes). At minimum for the affected surface:
   - Rust server: `cd apps/server && cargo build && cargo fmt --check && cargo clippy -- -D warnings && cargo test`
   - Reader/admin: `pnpm check` (svelte-check) in the app, or the repo-root check script — confirm the exact script from `package.json` / `.github/workflows/ci.yml`.
   - Worker: `cd apps/worker && npx tsc --noEmit`.
   - Add or extend a unit test when the fix has testable logic (rate limiters, dedup, gating, aggregation).
6. **Commit** the single fix locally with a message referencing the ID
   (e.g. `fix(auth): expire sessions [A1]`). **Do not push or open a PR unless the user asks.**
7. **Tick the checkbox** below and add a one-line note (what changed / any follow-up). Keep this file honest.
8. **Do not deploy, do not run servers against production, do not commit secrets.**

**Severity legend** (from the audit, post-verification): 🔴 high · 🟠 medium-high · 🟡 medium · ⚪ low.
Phases are ordered by risk-to-ship, then by subsystem to minimize context-switching and merge conflicts.

---

## Progress checklist

**Phase 1 — Security must-fix before any public exposure**
- [x] 1.1 🔴 D1 — deploy.sh must halt on default admin password — deploy.sh now `die`s after creating `.env` (no fall-through to up+bootstrap); bootstrap.py `main()` exits 1 if `KOMIKA_ADMIN_PASSWORD` == `change-this-admin-pw`. Verified both exit codes.
- [x] 1.2 🔴 A2 — stop trusting client `X-Forwarded-For` — hand-rolled `Cidr` (v4/v6 prefix match, no new crate); `resolve_client_ip` now honors XFF/X-Real-IP only when the socket peer is in `TRUSTED_PROXY_CIDRS` (new Config field, default empty → uses socket peer). v4-mapped-v6 canonicalized. 9 unit tests added. Documented the knob in docker-compose.yml.
- [x] 1.3 🔴 A1 — session expiry + rotation — migration `0010` adds `sessions.expires_at` (+ index, backfill via strftime to canonical `YYYY-MM-DDTHH:MM:SSZ`). `new_session` sets `expires_at = now + SESSION_TTL_SECS` (new Config field, default 30d) using a fixed-width lexically-sortable format (`auth::format_ts`), + opportunistic GC of expired rows. `user_for_token` enforces `AND s.expires_at > ?`. Test seeds an expired token → `session` returns null. Note: rotation-on-use not added (absolute TTL only); localStorage token (A6) still applies.
- [x] 1.4 🟡 D2 — restrict/loopback-bind unauthenticated Suwayomi — chose **proxy mode + loopback bind** (user decision). Suwayomi port now `127.0.0.1:4567` (internal federation unaffected); reader defaults `IMG_MODE=proxy` + new `PUBLIC_KOMIKA_IMG_WORKER` build arg wired to `KOMIKA_IMG_WORKER_URL` (.env.example + Dockerfile); CSP img-src → Worker origin. `docker compose config` OK. **Caveat (documented in compose):** Worker can't reach loopback Suwayomi, so *Suwayomi-source* (non-canonical) images won't proxy — canonical MangaDex path unaffected. Images stay broken until operator sets `KOMIKA_IMG_WORKER_URL`.
- [x] 1.5 🟡 D4 — disable public GraphiQL + introspection in prod — new `graphiql_enabled` Config bool (`GRAPHIQL_ENABLED`, default false). GET `/graphql` mounts GraphiQL only when enabled (else POST-only → 405 on GET). `build_schema(state, disable_introspection)` calls `.disable_introspection()` when the flag is off → `__schema` resolves to null (no leak). Wired `GRAPHIQL_ENABLED=off` into compose server env. Test asserts introspection null-when-off / real-when-on.
- [x] 1.6 🟡 A5 — reserve `KOMIKA_ADMIN_USERS` names from open registration — **server-side provisioning** (user chose most-secure). `register` now rejects configured admin usernames ("This username is reserved.") and never inline-grants admin. New `provision_admins()` (only path to admin): at startup creates a missing admin from `KOMIKA_ADMIN_PASSWORD` (Config `admin_password`, redacted via `Secret`) else promotes existing (never re-passwords). Compose passes `KOMIKA_ADMIN_PASSWORD`/`KOMIKA_ADMIN_EMAIL` to server; bootstrap.py's register-based create_admin removed (now informational). 2 tests added (provision idempotency/create/promote; reserved-name rejection).
- [x] 1.7 🟡 A3 / A4 / ⚪ A7 — auth hardening bundle — **A3:** login always runs an Argon2 verify (real hash or fixed `DUMMY_PASSWORD_HASH`) so timing doesn't reveal username existence. **A4:** `is_limited` no longer inserts on read (`get_mut`+`?`), evicts fully-stale keys, and `record` sweeps the map when it exceeds 4096 keys. **A7:** `MAX_PASSWORD_LEN=1024` enforced before hashing in both login (→ invalid creds) and register (→ "at most 1024"). 3 tests added. Note: the distinct "This account has been suspended." message (A3 second half) left as-is — intended UX, only reachable with a correct password (not a username-enumeration vector).

**Phase 2 — MangaDex proxy-compliance (avoid 429/IP ban)**
- [x] 2.1 🔴 M1 / CR1 — dedicated ≤40/min limiter for `/at-home` — `MangaDexClient` gains an `athome_limiter: TokenBucket` (rate `MANGADEX_ATHOME_PER_MIN/60`, Config default 40); `at_home` now acquires it in addition to the global bucket. **Caught + fixed a latent bug:** `TokenBucket` capacity was `= rate`, so a sub-1/s rate (0.67/s) could never reach the 1-token threshold and would block forever — floored capacity at 1.0 (no-op for rate≥1). `MangaDexClient::new` gained an arg → updated main.rs + 4 test sites. Fixed the inverted comment. 3 tests added. **Deferred (optional, noted):** per-chapter at_home TTL cache — not required for the 40/min cap; skipped to keep scope tight.
- [x] 2.2 🟠 M2 — 429/5xx backoff + Retry-After; target ~4 req/s — new `get_with_retry` helper: bounded retries (MAX 4) on 429/5xx, honors `Retry-After` (integer secs, clamped ≤60s) else exponential backoff (0.5→4s); re-acquires the limiter(s) each attempt so retries still cost budget. Wired into `list_manga`/`list_chapters`/`at_home` (replacing the `?`-abort on non-success). Default `mangadex_rate_per_sec` 5.0→4.0. 2 tests (`retry_after` parse/clamp, `backoff` curve). Note: page still errors after exhausting retries (persistent failure should surface); the resumable-seed half is M3/M6 (item 2.4).
- [x] 2.3 🟡 M5 — HTTP request timeout on the MangaDex client — client builder now sets `.connect_timeout(10s)` + `.timeout(30s)` (was only `.user_agent`). reqwest's timeout is on by default (verified builds/tests). A timeout surfaces as a request error (not retried like a 429/5xx); incremental cycles self-heal since the cursor only advances on success.
- [x] 2.4 🟡 M3 / M6 — resumable seed + gate chapters on catalogue completion — migration `0011` adds `catalogue_sync_state.seed_done` (existing rows backfilled to 1). New catalog API: `SyncState`, `get_sync_state`, `set_seed_progress` (provisional, seed_done=0), `mark_seed_done`. **M6:** `sync_catalogue`/`sync_chapters` checkpoint the provisional `createdAt` cursor on each window slide, so an aborted seed resumes from the last window instead of createdAt=0. **M3:** `sync_cycle` gates the chapters job on the catalogue's `seed_done` (re-reads after the catalogue arm so a same-cycle completion is seen) — old chapters no longer skipped-then-lost. Replaced `get_sync_cursor` with `get_sync_state`; rewrote its test. **Residual noted:** per-record work upsert failures *during* the seed still leave those works uncatalogued (chapters for them skipped); not covered by gating.
- [x] 2.5 🟡 M4 — fleet-limiter decision — **decided: document single-replica-for-sync** (a shared DB/Redis limiter is a large lift + Redis isn't in the stack; SQLite/Litestream already implies single-node). Added a "Single-replica constraint (M4)" paragraph to CATALOGUE.md §9 and a FLEET CONSTRAINT comment at the `spawn_recurring` call site: the in-process TokenBucket only bounds one process, so exactly one replica may have `CATALOGUE_SYNC=on`; shared limiter is the prerequisite for multi-replica. Shipped single-container compose satisfies it.
- [x] 2.6 ⚪ M7 / M8 — boundary-second tiebreak logging + docstring fix — **M7:** the window-stuck guard in both `sync_catalogue` and `sync_chapters` now logs at `error!` (was `warn!`) with the `since` and an explicit note that >9,900 records sharing the boundary second means the overflow is dropped and a secondary tiebreaker would be needed. **M8:** corrected the `sync_catalogue` docstring — a failed *record* is skipped, but a page that still errors after the retry layer aborts the sweep (cursor unchanged, retried next cycle). No actual secondary tiebreaker implemented (pathological case; plan scoped this to logging).

**Phase 3 — The `addSourceSeries` cluster (wire + harden together)**
- [x] 3.1 🟡 C1 + 🟠 DD2 + 🟡 DD1 + 🟡 N1 + 🟡 N5 — wire+harden Tier-2 add flow (one commit). **C1:** `MatchResult` type → `ADD_SOURCE_SERIES` op → `addSourceSeries?` on Backend/GraphQLBackend → guarded `data.ts` wrapper → "Add" button on admin catalog rows (shows decision / errors). **DD2:** new `find_source_series` (id+work_id); resolver short-circuits with `decision:"existing"` before any `create_work` → no orphan work / no dup merge_candidate (tested both New and Review paths). **DD1:** new `SuwayomiClient::cover_bytes` (fetches via **internal** base_url, not `abs()`'s public url) → `phash::dhash` → fed to `Candidate.cover_phash` AND `WorkInput.cover_phash`; `external_ids` left empty (Suwayomi exposes none — commented). **N1/N5:** **took the genre-heuristic path (option 3)** — no live Suwayomi to probe `source.isNsfw`, and adding an unconfirmed field would break every manga query; OR'd into both `WorkInput.is_nsfw` and `upsert_source_series`. Extracted `add_source_series_core` (Suwayomi-free) for unit tests. 3 server tests + admin/reader `check` + build all green. **Follow-up:** admin catalog list may mostly show already-added series (idempotent re-add is a no-op); a source-browse UI is out of scope.
- [x] 3.2 🟡 N2 — gate the Suwayomi `series`/`chapters`/`pages`/`library` resolvers. `series`/`chapters` now short-circuit with `Error::new("No such series")` (mirroring the canonical `is_nsfw && !viewer_show_nsfw` pattern) **before** any source round-trip, so a hand-crafted sequential id can't leak detail/chapter-list. `pages` resolves the owning manga via a new `SuwayomiClient::chapter_manga_id` (Suwayomi chapters aren't mirrored locally) then gates identically (`"No such chapter"`); unknown chapters fall through to the cleanly-failing fetch, matching `canonicalPages`. `library` wraps its result in `filter_nsfw`. New test `suwayomi_detail_and_reader_paths_gate_nsfw` covers series/chapters deny + opt-in-passes + not-over-blocking. **Note:** the `pages`/`library` success + `pages` deny paths need a reachable Suwayomi and so aren't unit-tested (harness points at 127.0.0.1:1); the gate logic is identical to the covered `series`/`chapters` paths.
- [x] 3.3 🟡 DD3 / DD4 — dedup precision (one commit). **DD3:** a title-driven auto-merge now requires cover-pHash corroboration — `score_candidate` returns `Scored { score, phash_sim }`, and the decision auto-merges only when `score >= HIGH && phash_sim >= PHASH_CORROBORATION (0.8)`; a shared common title + description overlap alone routes to Review. Updated the old `title_plus_copied_description_auto_merges` test to `exact_title_with_description_only_goes_to_review` (its former behavior *was* the DD3 hole) and added `exact_title_with_cover_phash_auto_merges`. **DD4:** replaced `longest_token` with `top_tokens(norm_titles, 3)` and unioned the candidate sets across the top-3 longest tokens (`FUZZY_BLOCK_TOKENS`), so a discriminating token that isn't the longest still blocks the real work. Test `fuzzy_block_finds_work_when_discriminating_token_is_not_longest`. Note: the pHash bar (0.8) is a judgment call — conservative on cross-source cover variance, favoring Review over a wrong auto-merge, per DD3's "favor caution".
- [x] 3.4 ⚪ N3 / N4 — one commit. **N3:** `updates` now filters NSFW in SQL (a `NOT EXISTS` over the suwayomi `source_series`→`work` join, mirroring `canonical_updates`' `(? = 1 OR …)` shape) in both the id-selection and `COUNT(*)` queries, and drops the post-slice `filter_nsfw` — so `total`/`hasNextPage` count only visible rows (no under-filled-page-with-hasNext skew). Test `updates_total_excludes_nsfw_for_opted_out_viewer`. **Scope note:** `search`/`discovery` federate live from Suwayomi (no local SQL query to push a predicate into); `discovery` exposes no count fields (no skew) and `search` already returns `total: None`, so the only residue is a `search` page under-filling after `filter_nsfw` — an unavoidable federation artifact, left as-is. **N4:** new `catalog::mark_work_nsfw` (escalate-only `UPDATE … WHERE is_nsfw = 0`); the add flow OR's `source_nsfw` into the target `work.is_nsfw` after upsert, closing the `auto_merge`-onto-SFW-work gap (`new`/`review` already mint the work with the flag). Test `auto_merge_ors_source_nsfw_into_existing_work`.
- [x] 3.5 ⚪ DD5 / DD6 / DD7 — one commit. **DD5:** renamed "MinHash" → exact shingle-Jaccard in `similarity.rs` module docs and CATALOGUE.md §4/§10 (chose the honest-rename route — the code computes exact Jaccard over full shingle sets, no MinHash signatures; MinHash would only approximate it at scale we don't have). **DD6:** the scored path now emits `title_exact` (was `title_corroborated`) so `method` labels match the documented `merge_candidate.method` enum in migration 0005 (`external_id/title_exact/fuzzy/description/cover`); the finer description/cover split isn't surfaced separately — noted in a code comment. No test asserted the old label. **DD7:** the best-candidate pick now breaks score ties by lowest `work_id` (was first-seen over a nondeterministic `HashSet`), so the surfaced Review candidate is stable across runs when equal-scoring works share the exact title. Test `tied_review_candidate_is_deterministic` (3 repeats, asserts the min work_id). **Phase 3 fully closed.**

**Phase 4 — Image pipeline**
- [x] 4.1 🟡 I1 / CR5 — `hostAllowed` now **fails closed** on an empty allowlist (`return false`, was `return true`), so an unconfigured Worker is never an open proxy; shipped `ALLOWED_SOURCE_HOSTS = "uploads.mangadex.org,mangadex.network"` as the committed default (the canonical web path) and updated the wrangler.toml + top-of-file comments (empty now denies all; add hosts for dev instead of clearing). Chose the always-fail-closed route over an `[env.production]`/`--env` restructure to avoid renaming the deployed worker. `tsc --noEmit` clean. **Note:** `originAllowed` (hotlink) intentionally left empty=disabled — it's not the SSRF control (I5); the host allowlist carries abuse prevention.
- [x] 4.2 🟡 I2 — hardened native `fetch_image` against client-side SSRF. Now: a shared `reqwest::Client` with a 30s timeout and `redirect::Policy::none()`; `validate_image_url` requires http(s) and resolves the host (via `tokio::net::lookup_host`, handling IP literals + names) rejecting any address that is loopback/private/link-local (incl. `169.254.169.254`)/CGNAT/unspecified/broadcast or their IPv6/v4-mapped forms; redirects followed **manually** up to 5 hops, re-validating each hop; body bounded by Content-Length fast-fail + streamed 32 MiB cap. Added `tokio` (net) dep + dev-dep (macros/rt). 6 unit tests (IPv4/IPv6 block+allow, scheme reject, loopback/LAN literal reject); `cargo test` + `cargo clippy` clean. **Residual (noted):** a hostname could DNS-rebind between the guard's resolution and reqwest's own — the guard blocks every static payload in the finding but not a rebinding attacker; full protection needs pinning the resolved IP into the connection (out of scope). Style: matched the crate's 2-space indent (Tauri scaffold), not rustfmt 4-space.
- [x] 4.3 🟡 I3 — the Worker now asserts the upstream `Content-Type` is `image/*` (new `isImageContentType`, tolerant of `; charset=` params) before re-serving; a non-image body is rejected `502 "Upstream is not an image"` instead of being laundered under our origin with permissive CORS + immutable cache. Chose reject over the coerce-to-attachment fallback — this is an image proxy, non-images have no legitimate path. `tsc --noEmit` clean.
- [x] 4.4 ⚪ I4 / I6 — **I4:** the native client now sends a descriptive `User-Agent` (`Komika/0.1 (+https://komika.app)`) and a per-request `Referer` (new `referer_for` → `scheme://host[:port]/`, mirroring the Worker), so hosts that gate on UA/Referer (MangaDex) no longer 403 the desktop/mobile build — closes the web/native divergence. Unit test `referer_is_origin_with_trailing_slash`. **I6:** `WebImageProvider` warns once (dev console) when `PUBLIC_KOMIKA_IMG_MODE=direct` is paired with a proxy-required host (`*.mangadex.org`/`*.mangadex.network`), instead of silently serving CORS-broken images. `cargo test`/`clippy` + reader `svelte-check` (0 errors) clean.
- [x] 4.5 ⚪ I7 — rewrote `apps/worker/README.md` to the workers-only reality (dropped the entire `cache=1`/B2 read-through/write-through/SigV4/secrets/"fails open" story; documents edge-cache-only, empty-allowlist-fails-closed, `image/*` enforcement) and updated SPEC.md: the image-pipeline bullet (`:351`), the platform table image-source cell (`:17`), the native-first note (`:25`), the scanner "B2 cache-fill" line (`:107`), and the roadmap checklist item (`:435`). Left the accurate Litestream→S3/B2 **backup** mentions (`:403`, `:80-81`) intact — B2/R2's only remaining job. `.dev.vars.example` was already correct. **Phase 4 fully closed.**

**Phase 5 — Scanner correctness**
- [ ] 5.1 🟡 SC1 — implement overdue re-poll cadence (`poll_every_minutes`)
- [x] 5.2 🟡 SC3 — extracted `record_scan` from `scan_series` (fetch stays in `scan_series`; the read-prior→detect→upsert is now a testable pool-only helper). First observation (`scan_state` returns `None`) records the baseline `known_chapter_count` with `last_new_chapter_at` left NULL and `new_found=false`, so a fresh deploy's first tick no longer floods `updates`; steady-state behaviour unchanged. 2 DB tests (baseline no-flag; subsequent add flags).
- [ ] 5.3 🟡 SC4 — detect add+remove churn (persist/compare max chapter number or key-set)
- [ ] 5.4 🟡 SC5 — minimum interval clamp
- [ ] 5.5 ⚪ SC6 / SC7 — scan-state transaction + `next_scan_at` consistency

**Phase 6 — Canonical reader path**
- [ ] 6.1 🟡 CR2 — deterministic English-scanlation selection
- [ ] 6.2 🟡 CR3 — reject/handle backfilled `w_<numeric>` ids
- [ ] 6.3 ⚪ CR4 — number-less chapter ordering (server/reader agreement)
- [ ] 6.4 🟡 CR6 — per-user progress/library/rating for canonical works (larger; tracked follow-up)

**Phase 7 — Social, admin, contract polish**
- [ ] 7.1 🟡 S1 — make `banCommenter` honest (cascade-hide, or drop the client-only filter)
- [ ] 7.2 🟡 AD1 — nullable poll-override so Save doesn't pin `30`
- [ ] 7.3 ⚪ AD2 — atomic `resolveMergeCandidate` (WHERE status='pending')
- [ ] 7.4 ⚪ C2 / C3 — `banUser` return shape; guard `setLibraryMark` for `w_` ids
- [ ] 7.5 ⚪ S2 / S3 / S4 / AD3 — social/admin low-severity cleanup

**Phase 8 — Deploy/ops hardening + doc reconciliation**
- [ ] 8.1 🟡 D3 — handle SIGTERM in graceful shutdown
- [ ] 8.2 🟡 D5 — loud "NO BACKUP CONFIGURED" banner in deploy.sh
- [ ] 8.3 🟡 D6 — document `CATALOGUE_SYNC`/`COVER_PHASH`/interval/UA in `.env.example`
- [ ] 8.4 ⚪ D7 / D8 — CI gates the deploy path; deploy.sh dies if unhealthy
- [ ] 8.5 ⚪ SC2 doc note — clarify the Suwayomi-vs-MangaDex monitoring split is intended

---

## Phase details

### Phase 1 — Security must-fix before any public exposure
Rationale: these gate a safe public launch and are mostly independent, low-risk, high-value.

- **1.1 D1 — deploy.sh must halt on default admin password.** `deploy/deploy.sh:32-36` copies
  `.env.example`→`.env` and warns but falls through to `up`+`bootstrap`. Make it `exit` after creating
  `.env`; make `bootstrap.py` (`:42,131-144`) hard-fail if `KOMIKA_ADMIN_PASSWORD` == `change-this-admin-pw`.
  Verify: run `bash -n deploy/deploy.sh`; dry-read the control flow.
- **1.2 A2 — stop trusting client XFF.** `resolve_client_ip` (`main.rs:49-65`) reads the leftmost
  `X-Forwarded-For` then `X-Real-IP` unconditionally, socket peer only as last resort; it's called at
  `main.rs:92` and the value keys the auth limiter. Add `TRUSTED_PROXY_CIDRS: Vec<String>` to `Config`
  (CSV parse, mirror `cors_origins` at `config.rs:61-68`); thread it into `resolve_client_ip(headers, peer,
  &trusted)` so XFF/X-Real-IP are honored ONLY when `peer.ip()` is inside a trusted CIDR, else use
  `peer.ip()`. **Prereq:** CIDR matching needs a crate — check `apps/server/Cargo.toml` for `ipnetwork`/`cidr`
  first; if absent, add one (or hand-roll IPv4/IPv6 prefix match). Config must reach the handler (it's in the
  router state today). **Test note:** `exec()` sets `ClientIp` directly and never calls `resolve_client_ip`,
  which is module-private in `main.rs` — unit-test it as a free fn inside `main.rs`, not via the gql harness.
  This is the keystone of the auth-rate-limit fix.
- **1.3 A1 — session expiry.** `sessions` (`0001_init.sql:14-18`) has token/user_id/created_at, no TTL;
  `user_for_token` (`auth.rs:49-58`) is the single validation choke point. **Add an `expires_at` column**
  (migration `0010`) rather than comparing `created_at` — `Utc::now().to_rfc3339()` emits a `+00:00` offset +
  nanoseconds while tests use `Z`/second precision, so a lexical `created_at > ?` compare is unsafe. Set
  `expires_at` in `new_session` (`mod.rs:1686`, e.g. now + `SESSION_TTL_SECS` default 30d, added to `Config`);
  enforce `AND s.expires_at > ?` (bind `Utc::now().to_rfc3339()`) in the `user_for_token` JOIN; GC expired rows
  on login or a periodic sweep. Verify: the harness seeds 2020-dated sessions (`mod.rs:1769`) — after the fix an
  expired token must make `{ session { user } }` return null; add a test seeding an old `expires_at`.
- **1.4 D2 — Suwayomi exposure.** Prefer defaulting `PUBLIC_KOMIKA_IMG_MODE=proxy` (Worker path) and
  binding `4567` to `127.0.0.1` in `deploy/docker-compose.yml:33-36`; or ship a reverse proxy that allows
  only thumbnail/page routes. Keep it consistent with the SUWAYOMI_URL/PUBLIC_URL split.
- **1.5 D4 — GraphiQL/introspection.** There is **no existing prod/dev signal** in the codebase (only ad-hoc
  flags like `catalogue_sync_enabled`). Least-invasive, consistent path: add a single bool `graphiql_enabled`
  to `Config` (default `false`, on/1/true parse like `config.rs:90-95`). In `main.rs:204-207` mount
  `get(graphiql)` only when enabled (else a 404 handler); add a `disable_introspection` bool param to
  `build_schema` (`mod.rs:110-115`) and call `.disable_introspection()` on the builder when the flag is off.
  Wire the flag into `deploy/docker-compose.yml` server env so the deployed stack is locked down by default.
- **1.6 A5 — admin-name reservation.** In `register` (`mod.rs:1284-1287`), don't grant `is_admin` on
  first registration of a configured name; reserve configured `KOMIKA_ADMIN_USERS` from open registration.
- **1.7 A3/A4/A7 bundle.** A3: run a dummy Argon2 verify on the missing-user path (`mod.rs:1230`).
  A4: drop empty rate-limiter map entries after `retain` (`mod.rs:48-69`). A7: cap password length ≤1024
  before hashing. Small, same file — one commit is fine.

### Phase 2 — MangaDex proxy-compliance
Rationale: violations here get the fleet-shared egress IP 429'd or banned; all in `apps/server/src/mangadex.rs`.

- **2.1 M1/CR1** — the `MangaDexClient` has ONE global `TokenBucket` (`mangadex.rs:55-115`) shared by all
  methods. Reuse the struct as-is: add a second field `athome_limiter: TokenBucket` constructed
  `TokenBucket::new(per_min / 60.0)` (40/min → 0.66/s), and `self.athome_limiter.acquire().await` inside
  `at_home` (`:226-235`) in addition to the global one. Add `MANGADEX_ATHOME_PER_MIN` (default 40) to `Config`.
  **`MangaDexClient::new` gains an arg → update BOTH call sites: `main.rs:160-163` and the test at
  `graphql/mod.rs:1779`.** Optionally add a small TTL cache (`Mutex<HashMap<String,(Instant,Vec<String>)>>`,
  at_home URLs are time-limited) — there is no at_home cache today. Fix the inverted comment (`:224-225`).
- **2.2 M2** — on 429/503 honor `Retry-After` + exponential backoff and retry the page instead of `?`-aborting
  (`:148-150/192-194/233-235/523`); lower the default `mangadex_rate_per_sec` to ~4 (`config.rs:96-100`).
- **2.3 M5** — set `.timeout(...)`/`.connect_timeout(...)` on the client builder (`mangadex.rs:107-110`, which
  currently sets only `.user_agent`). Confirm reqwest's timeout feature is enabled (it's on by default).
- **2.4 M3/M6** — gate `sync_chapters` on a completed catalogue seed (or re-seed chapters when the catalogue
  finishes its first full crawl), and persist the in-progress `since` window as a provisional cursor so an
  aborted seed resumes near where it stopped. Together they close the "permanently missed chapters" hole.
- **2.5 M4** — decide: document single-replica-for-sync, or move the token budget to a shared limiter keyed
  by egress IP. At least document the constraint.
- **2.6 M7/M8** — log the boundary-second drop at error with a tiebreak note; fix the docstring that claims
  page failures don't abort.

### Phase 3 — The `addSourceSeries` cluster
Rationale: **C1 (no client binding) makes DD1, DD2, N1, N5 latent. The moment you wire the mutation to a
client, all four go live.** Fix them in the same change so you never ship a live-but-broken add flow.

- **3.1 (do together) → dedicated prompt: [`docs/audit-fixes/PROMPT-3.1-add-source-series.md`](docs/audit-fixes/PROMPT-3.1-add-source-series.md).**
  This is a large, cross-cutting change (server + `@komika/api` + `@komika/types` + admin UI). The prompt is a
  full self-contained guide with every struct/signature, the DD2 idempotency pre-check, the cover-pHash helper
  to add, and — importantly — a **pre-decided NSFW path**: probe the live Suwayomi schema for `source.isNsfw`,
  and if absent fall back to a genre-based heuristic (Suwayomi exposes no external tracker IDs, so DD1's
  `external_ids` half is a documented no-op; cover-pHash is the actionable part). Summary of the four:
  - **C1** — `MatchResult` type + `ADD_SOURCE_SERIES` op + `addSourceSeries?()` on `Backend`/`GraphQLBackend` +
    an "Add" button on existing admin catalog rows (`apps/admin/src/routes/+page.svelte`, rows already carry
    `s.id` = Suwayomi manga id).
  - **DD2** — short-circuit on the existing-but-unused `find_source_series_id` (`catalog/mod.rs:590`) at the top
    of `add_source_series` before any `create_work`.
  - **DD1** — add a `cover_bytes` helper to `SuwayomiClient` + `phash::dhash`; feed `Candidate.cover_phash` and
    `WorkInput.cover_phash`.
  - **N1/N5** — fetch source nsfw (probe `source { isNsfw }`, else genre heuristic); OR into both
    `make_work().is_nsfw` and the `upsert_source_series` arg (both hardcoded `false` today, `mod.rs:1534/1588`).
- **3.2 N2** — mirror the canonical gate onto `series`/`chapters`/`pages` (return not-found when
  `is_nsfw && !viewer_show_nsfw`) and `filter_nsfw` in `library` (`mod.rs:872-906`). Sequence **after** 3.1
  so the flag is actually being set by then.
- **3.3 DD3/DD4** — require cover-pHash corroboration (not description-only) for exact-title auto-merge, or
  route ultra-common normalized titles always to Review; block on top-N longest tokens instead of one.
- **3.4 N3/N4** — push the nsfw predicate into SQL for `updates`/`search`/`discovery` counts; ensure the
  source flag is OR'd into `work.is_nsfw` (not just `source_series`).
- **3.5 DD5/DD6/DD7** — rename "MinHash"→shingle-Jaccard (or implement MinHash), align `method` labels to the
  migration enum, add a deterministic tiebreak in the Review candidate pick.

### Phase 4 — Image pipeline
- **4.1 I1/CR5** — ship a non-empty prod `ALLOWED_SOURCE_HOSTS`/`ALLOWED_ORIGINS` (via an `[env.production]`
  block) including `uploads.mangadex.org,mangadex.network`, or make an empty list fail-**closed** in prod
  (`wrangler.toml:22,31`, `index.ts:155-157`).
- **4.2 I2** — in `apps/reader/src-tauri/src/lib.rs:8-15`: validate the URL host against an allowlist, use a
  shared `reqwest::Client` with a timeout, re-validate each redirect hop (or disable redirects), reject
  non-http(s), cap body size.
- **4.3 I3** — reject/coerce non-`image/*` upstream responses in the Worker (`index.ts:108,121`).
- **4.4 I4/I6** — set a Referer + descriptive UA on the native fetch; warn/guard when `direct` mode is paired
  with a backend whose hosts require proxying.
- **4.5 I7** — update `apps/worker/README.md` + `SPEC.md:351-353` to drop the removed `cache=1`/B2 path.

### Phase 5 — Scanner correctness (`apps/server/src/scanner.rs`)
- **5.1 SC1** — track an "awaiting update" state and, once overdue with no new chapter, schedule the next scan
  at `poll_every_minutes` (clamped) until a new chapter lands, instead of a full interval (`:248-261`).
- **5.2 SC3** — on first observation (no prior row), record the baseline `known_chapter_count` without setting
  `last_new_chapter_at` (`:238-245`).
- **5.3 SC4** — persist and compare the max chapter number (or a chapter-key set), not just count, so add+remove
  churn and number regressions are detected (needs a migration column).
- **5.4 SC5** — add `MIN_INTERVAL_HOURS` and clamp the effective interval into `[MIN, MAX]`.
- **5.5 SC6/SC7** — wrap the scan-state read-modify-write in a transaction; make `next_scan_at` and live gating
  agree.

### Phase 6 — Canonical reader path
- **6.1 CR2** — add `ORDER BY` to `load_canonical_chapters` and a deterministic tiebreak in
  `select_reader_chapters` even when both candidates are English (`catalog/mod.rs:282-342`).
- **6.2 CR3** — return "no such work" from `canonicalSeries`/`canonicalChapters` when `mangadex_id` is `None`,
  or gate `isCanonicalId` routing on a mangadex-anchored flag (`source.ts:30-32`).
- **6.3 CR4** — keep server ordering client-side (don't re-sort, or sentinel-sort number-less last).
- **6.4 CR6 → dedicated prompt: [`docs/audit-fixes/PROMPT-6.4-canonical-progress.md`](docs/audit-fixes/PROMPT-6.4-canonical-progress.md).**
  Per-user progress/library/rating for canonical works. Smaller than it looks: **ratings reuse the `reviews`
  table with zero schema change** (opaque `series_id`; just call `rating_summary` from `map_canonical_series`);
  progress/library need one `0010` migration + two tables; the only reader edit is relaxing the `saveProgress`
  `/^\d+$/` guard (`source.ts:481`); **no contract/types change** (the string-id contract already carries
  `w_`/uuid ids). **Scope boundary baked into the prompt:** the Library *screen* (`getLibrary` merge) is a
  deferred follow-up, NOT part of CR6 — the change covers the series-page + reader round-trip only.

### Phase 7 — Social, admin, contract polish
- **7.1 S1** — either cascade-hide banned users' comments (add `AND u.is_banned = 0` to the comments/reviews
  JOIN, or soft-delete on ban) or remove the client-only filter so the UI is honest (`CommentThread.svelte:108-116`).
- **7.2 AD1** — expose a nullable `pollEveryMinutesOverride` and decode the editor field from it (blank when unset)
  so Save doesn't pin `30` (`mod.rs:344`, `+page.svelte:61`).
- **7.3 AD2** — `UPDATE merge_candidate SET status=… WHERE id=? AND status='pending'`, treat 0 rows as
  already-resolved (`mod.rs:1630-1680`).
- **7.4 C2/C3** — return `AdminUser!` from `banUser`; early-return the optimistic value in `setLibraryMark` when
  `isCanonicalId(seriesId)` (`source.ts:461-470`).
- **7.5 S2/S3/S4/AD3** — model or remove ephemeral likes/replies + persist local spoiler flag; add a "my review"
  query; delete the dead `SeriesComment`/`ReaderComment` interfaces; give the updates pager a real `hasNextPage`.

### Phase 8 — Deploy/ops hardening + doc reconciliation
- **8.1 D3** — add a `unix::SignalKind::terminate()` branch to `shutdown_signal` (`main.rs:288-292`).
- **8.2 D5** — print a prominent "NO BACKUP CONFIGURED" banner at the end of `deploy.sh` when `LITESTREAM_*`
  is unset.
- **8.3 D6** — document `CATALOGUE_SYNC`/`COVER_PHASH`/`CATALOGUE_SYNC_INTERVAL_SECS`/`MANGADEX_USER_AGENT` in
  `deploy/.env.example` with the rate-limit caveat; decide the default-stack posture.
- **8.4 D7/D8** — add a CI job that `docker build`s both Dockerfiles + `docker compose config`; make `deploy.sh`
  `die` with the offending service names if still unhealthy after the wait.
- **8.5 SC2 note** — add a short clarifying line to CATALOGUE.md/SPEC.md that adaptive scanning is Suwayomi-only
  by design and MangaDex works update via `canonicalUpdates` (this was verified as intended, not a bug).

---

## Notes for the implementer

- **Line numbers drift.** After each committed fix, later IDs' `file:line` in AUDIT_FINDINGS.md may be stale by
  a few lines — search for the quoted code, don't trust the number.
- **Don't widen scope silently.** If a fix reveals an adjacent issue not in AUDIT_FINDINGS.md, note it in your
  checklist line rather than folding it in unannounced.
- **The `addSourceSeries` cluster (Phase 3.1) is the one place NOT to split fixes** — wiring the client without
  the idempotency/NSFW fixes ships a live regression. Everything else is one-at-a-time.
- **Tests are the verification of record** for the server-side logic fixes (limiters, dedup, gating, session
  expiry, aggregation). Prefer adding a failing test first where practical.
- **Keep this checklist current** — it is the cross-session source of truth for what's done.
