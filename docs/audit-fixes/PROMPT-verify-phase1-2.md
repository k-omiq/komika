# Verification Prompt — Phase 1 (security) + Phase 2 (MangaDex compliance)

> **Self-contained kickoff prompt for an independent verification session.** Phases 1 and 2 of the
> Komika audit are implemented and merged into `main` (13 fix commits). Your job is to
> **independently confirm each fix does what it claims, that the tests actually exercise it, and that
> nothing regressed** — not to re-implement. Evidence base: [AUDIT_FINDINGS.md](../../AUDIT_FINDINGS.md);
> what was changed + why: [AUDIT_FIX_PLAN.md](../../AUDIT_FIX_PLAN.md). Repo root: `/Users/caved/dev/komika`.

Paste everything below the line into a fresh session.

---

You are **verifying** Phases 1 and 2 of the Komika audit remediation at `/Users/caved/dev/komika`.
Work **read-only on `main`** (already contains all the fixes). Do NOT rewrite the fixes. If you find a
genuine defect, STOP and report it with evidence (file:line, the failing scenario, and a minimal
repro); only then, if asked, fix it on a branch (`audit-fixes/verify-fixups`) one commit per issue.

Be adversarial: for each item, (a) read the fix against its finding, (b) confirm a test exists and
actually asserts the fixed behavior (not a tautology), and (c) where it's not unit-tested, exercise it
or reason concretely about the gap. Produce a short PASS/CONCERN table at the end.

## Baseline (run first)
```
cd apps/server && cargo build && cargo fmt --check && cargo clippy -- -D warnings && cargo test
```
Expect a clean build, no clippy warnings, and ~73 passing tests. Note the exact count. Also:
```
cd deploy && bash -n deploy.sh && python3 -m py_compile bootstrap.py && docker compose config >/dev/null && echo COMPOSE_OK
```
(`docker compose config` needs docker; if unavailable, validate the YAML with python and say so.)

## Commit map (git log --oneline on main)
- `[D1]` deploy: halt on default admin password
- `[A2]` auth: only trust X-Forwarded-For from configured proxies
- `[A1]` auth: expire sessions
- `[D2]` deploy: loopback-bind Suwayomi, route images via Worker
- `[D4]` api: disable public GraphiQL + introspection by default
- `[A5]` auth: reserve admin usernames; provision admins server-side
- `[A3][A4][A7]` auth: timing, limiter eviction, password cap
- `[M1][CR1]` mangadex: dedicated 40/min limiter for /at-home
- `[M2]` mangadex: retry 429/5xx with backoff; lower default rate
- `[M5]` mangadex: request + connect timeouts
- `[M3][M6]` mangadex: resumable seed + gate chapters on catalogue seed
- `[M4]` docs: single-replica sync constraint
- `[M7][M8]` mangadex: error-log boundary-second drop; docstring fix

Use `git show <hash>` per item.

---

## Phase 1 — checks

- **D1** — `deploy/deploy.sh`: confirm it `die`s (exits non-zero) after creating `.env` and does NOT
  fall through to `up`/`bootstrap`. `deploy/bootstrap.py` `main()` must exit 1 when
  `KOMIKA_ADMIN_PASSWORD == "change-this-admin-pw"`. Repro: `KOMIKA_ADMIN_PASSWORD=change-this-admin-pw
  python3 deploy/bootstrap.py; echo $?` → 1 before touching the stack. Try a real pw → passes the guard.
- **A2** — `resolve_client_ip(headers, peer, trusted)` in `main.rs`: XFF/X-Real-IP honored **only** when
  `peer` ∈ `TRUSTED_PROXY_CIDRS`, else socket peer. Confirm the `Cidr` matcher (v4/v6, prefix mask,
  v4-mapped-v6 canonicalization) and that the 9 unit tests actually assert spoof-is-ignored and
  trusted-honors-XFF. Sanity-check the CIDR math (e.g. `10.0.0.0/8` contains `10.255.255.255`, excludes
  `11.0.0.1`).
- **A1** — migration `0010_session_expiry.sql` adds `expires_at` (+ index, backfill). `new_session`
  stamps `now + SESSION_TTL_SECS` in the canonical `%Y-%m-%dT%H:%M:%SZ` format; `user_for_token`
  enforces `AND s.expires_at > ?`. **Critically verify the format is lexically sortable and the compare
  is safe** (the whole point of the column-not-string-compare choice). Confirm the seeded-expired-token
  test asserts `session` → null. Spot-check: is there any code path that inserts a session without
  `expires_at`? (grep `INSERT INTO sessions`.)
- **D2** — `docker-compose.yml`: Suwayomi port is `127.0.0.1:...:4567`; reader `IMG_MODE=proxy` +
  `PUBLIC_KOMIKA_IMG_WORKER` build arg; CSP img-src → Worker. Confirm the documented caveat (Worker
  can't reach loopback Suwayomi → Suwayomi-source images won't proxy; canonical MangaDex path is fine)
  is accurate and that internal federation (`suwayomi:4567`) is untouched.
- **D4** — `graphiql_enabled` Config flag (default false): GET `/graphql` mounts GraphiQL only when on
  (else POST-only → 405 on GET); `build_schema(state, disable_introspection)` calls
  `.disable_introspection()` when off. Verify the test asserts `{ __schema { queryType { name } } }`
  → `{"__schema":null}` when disabled and real schema when enabled. Confirm compose sets
  `GRAPHIQL_ENABLED=off`.
- **A5** — `register` rejects any `KOMIKA_ADMIN_USERS` name (case-insensitive) and never inline-grants
  admin. `provision_admins` at startup is the SOLE admin path: creates a missing admin from
  `KOMIKA_ADMIN_PASSWORD` (redacted in Config `Debug` via `Secret`), else promotes existing, never
  re-passwords. **Adversarial checks:** (1) does `?cfg` logging leak the password? (grep the
  `tracing::info!(?cfg` site; confirm `Secret`'s Debug prints `<redacted>`). (2) Can a non-admin still
  become admin any way? (3) Does the multi-admin email synthesis avoid a UNIQUE collision? Confirm the
  provision-idempotency + reserved-name tests assert what they claim. Check `bootstrap.py` no longer
  registers the admin.
- **A3/A4/A7** —
  - A3: `login` runs an Argon2 verify on **every** path (real hash or `DUMMY_PASSWORD_HASH`) — confirm
    the missing-user branch actually calls `verify_password` (constant work), and the unknown-username
    test asserts the uniform error. (Note: the "account suspended" message is intentionally left; confirm
    it's only reachable with a correct password.)
  - A4: `is_limited` uses `get_mut` (no insert on read) + evicts fully-stale keys; `record` sweeps at
    >4096 keys. Confirm `limiter_does_not_leak_keys` asserts the map stays empty on reads and evicts on
    stale read (it reads the private `hits` map).
  - A7: `MAX_PASSWORD_LEN=1024` enforced **before** hashing in both login and register. Confirm the
    overlong-password test hits both.

## Phase 2 — checks

- **M1/CR1** — `at_home` acquires a dedicated `athome_limiter` (rate `MANGADEX_ATHOME_PER_MIN/60`,
  default 40) **in addition to** the global bucket. **Verify the TokenBucket capacity-floor fix**: for a
  sub-1/s rate, `capacity` is floored to 1.0 so `acquire` doesn't block forever — confirm the
  `sub_one_per_sec_bucket_does_not_hang` test would fail without the floor, and `capacity` stays == rate
  for rate ≥ 1. Confirm both call-site args (main + tests) pass `athome_per_min`.
- **M2** — `get_with_retry`: retries 429 + 5xx up to 4×, honors `Retry-After` (integer seconds, clamped
  ≤60s) else exponential backoff (0.5→…), **re-acquiring the limiter each attempt**. Confirm it's wired
  into `list_manga`/`list_chapters`/`at_home`, and that non-retryable 4xx and exhausted retries still
  `Err`. Check the `retry_after`/`backoff` unit tests. Confirm default `mangadex_rate_per_sec` is now 4.0.
- **M5** — client builder sets `connect_timeout(10s)` + `timeout(30s)`. Confirm reqwest's timeout is
  actually enabled (build proves it). Note: a timeout is NOT retried (it errors out of the `?`) — confirm
  the docstring says so and that's acceptable (incremental cycle self-heals).
- **M3/M6** — migration `0011` adds `catalogue_sync_state.seed_done` (existing rows backfilled to 1).
  **M6:** `sync_catalogue`/`sync_chapters` checkpoint a provisional cursor on each window slide
  (`set_seed_progress`, seed_done=0) → resume, not restart-from-0. **M3:** `sync_cycle` gates the chapter
  job on the catalogue `seed_done`, **re-reading state after the catalogue arm** so a same-cycle
  completion counts. Trace the state machine: fresh→seeding→(resume on abort)→done→incremental. Confirm
  `sync_state_seed_progress_and_completion` covers provisional-vs-done and that `set_sync_cursor` (used
  for incremental) preserves `seed_done=1`. **Known residual (confirm it's noted, not a surprise):**
  per-record upsert failures during the seed still leave those works uncatalogued.
- **M4** — CATALOGUE.md §9 + the `spawn_recurring` comment document the single-replica-for-sync
  constraint (in-process limiter). Confirm the doc is accurate and consistent with the SQLite/Litestream
  single-node reality.
- **M7/M8** — the boundary-second window-stuck guard logs at `error!` (with `since` + drop note) in both
  sweeps; the `sync_catalogue` docstring correctly says a record is skipped but a page that fails after
  retries aborts (cursor unchanged).

## Cross-cutting adversarial sweep
- Grep for any **new** hardcoded secrets, any `INSERT INTO sessions` missing `expires_at`, any
  `MangaDexClient::new` call site not updated, any `unwrap()`/`expect()` added on a network/parse path.
- Confirm no fix silently changed unrelated behavior (diff each commit is scoped to its claim).
- Re-run the full suite once more; confirm the count is stable and no test is `#[ignore]`d.

## Deliverable
A PASS / CONCERN table, one row per finding ID (D1, A2, A1, D2, D4, A5, A3, A4, A7, M1/CR1, M2, M5,
M3/M6, M4, M7/M8), with a one-line justification each and the exact `cargo test` count. List any CONCERN
with a concrete repro. Do not change code unless explicitly asked after reporting.
