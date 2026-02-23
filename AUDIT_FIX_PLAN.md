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
- [ ] 2.4 🟡 M3 / M6 — resumable seed + gate chapters on catalogue completion
- [ ] 2.5 🟡 M4 — fleet-limiter decision (single-replica doc, or shared limiter)
- [ ] 2.6 ⚪ M7 / M8 — boundary-second tiebreak logging + docstring fix

**Phase 3 — The `addSourceSeries` cluster (wire + harden together)**
- [ ] 3.1 🟡 C1 + 🟠 DD2 + 🟡 DD1 + 🟡 N1 + 🟡 N5 — wire the Tier-2 add flow to a client AND fix idempotency, the dead ladder rungs, and source-level NSFW ingestion **in the same change**
- [ ] 3.2 🟡 N2 — gate the Suwayomi `series`/`chapters`/`pages`/`library` resolvers
- [ ] 3.3 🟡 DD3 / DD4 — dedup precision (common-title guard, multi-token blocking)
- [ ] 3.4 ⚪ N3 / N4 — NSFW pagination-count skew + fold source flag into `work.is_nsfw`
- [ ] 3.5 ⚪ DD5 / DD6 / DD7 — dedup labels/determinism cleanup

**Phase 4 — Image pipeline**
- [ ] 4.1 🟡 I1 / CR5 — non-empty prod allowlist default (or fail-closed in prod)
- [ ] 4.2 🟡 I2 — validate/timeout/size-cap native `fetch_image`
- [ ] 4.3 🟡 I3 — Worker Content-Type validation
- [ ] 4.4 ⚪ I4 / I6 — native UA/Referer; guard `direct` mode
- [ ] 4.5 ⚪ I7 — reconcile stale worker/SPEC B2 docs

**Phase 5 — Scanner correctness**
- [ ] 5.1 🟡 SC1 — implement overdue re-poll cadence (`poll_every_minutes`)
- [ ] 5.2 🟡 SC3 — don't flag the back catalogue as "new" on first observation
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
