# Komika — Production readiness

Ops companion to [`SPEC.md`](./SPEC.md). SPEC describes the product surface and
architecture; this file tracks the concrete work to run Komika in production.
Legend: `[x]` done · `[~]` in progress / partial · `[ ]` TODO.

Deployment shape (see [`deploy/README.md`](./deploy/README.md)):
**reader / admin** = static SPAs on an edge/CDN host · **komika-server** = one
small always-on Rust container (SQLite on a persistent volume) · **suwayomi** =
private container beside the server · **images** = Cloudflare Worker + B2.

---

## Security

- [x] Password hashing with **argon2id** (`apps/server/src/auth.rs`).
- [x] Opaque session tokens via `Authorization: Bearer <token>`; server-side session→user lookup.
- [x] Admin authorization gate (`KOMIKA_ADMIN_USERS`, `updateSeriesAdmin` admin-only mutation).
- [~] **CORS** allow-list on the server (`CORS_ORIGINS`) — keep tight to real front-end origins in prod.
- [~] **Security headers** — being added (nginx sets `X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy` in `deploy/nginx.conf`; CSP stubbed pending final image/API origins).
- [ ] **TLS everywhere** — terminate at the edge (Fly/LB/Caddy); never expose server or Suwayomi over plain HTTP.
- [ ] **WAF / rate limiting / bot mitigation** — front public origins with Cloudflare or equivalent; add login/mutation rate limits.
- [ ] **Suwayomi not publicly exposed** — keep on the private network only (compose publishes 4567 for local debug — drop for prod).
- [ ] **Secrets management** — move `KOMIKA_ADMIN_USERS` and future keys to a platform secret store; never bake into images.
- [ ] Dependency scanning — `cargo audit` + `pnpm audit` / Dependabot in CI.
- [ ] Session hardening — expiry/rotation, logout-all, brute-force lockout.

## Observability

- [x] Structured logging via `tracing` + `tracing-subscriber` (env-filtered, `RUST_LOG`).
- [x] `tower_http` request tracing enabled.
- [ ] Log shipping to an aggregator (platform stdout → log service).
- [ ] Metrics endpoint (Prometheus/OTLP) — request rate, latency, error rate, Suwayomi federation health.
- [ ] Error tracking (Sentry or similar) for server + reader.
- [ ] Uptime/synthetic monitoring hitting `/health` and a key reader route.
- [ ] Dashboards + alerting (SLOs on availability/latency/error rate).

## Performance

- [x] Tiny server footprint — Rust release profile tuned for size (`opt-level=z`, LTO, `strip`, `panic=abort`); ~5–15 MB RSS.
- [x] Reader is a static SPA (`adapter-static`), CDN-cacheable; immutable-asset caching in `deploy/nginx.conf`.
- [~] Image delivery split via Cloudflare Worker + B2 (`apps/worker`) — separate workstream.
- [ ] **Core Web Vitals budget** — Lighthouse CI job is a stub (`workflow_dispatch`); add `lighthouserc.json` + `budget.json` (LCP/CLS/INP) and enforce.
- [ ] CDN in front of the SPA + long-cache headers verified in prod.
- [ ] Server-side caching of hot Suwayomi federation reads (catalog/series).

## Reliability

- [x] Graceful shutdown (`axum ... with_graceful_shutdown`, SIGINT/SIGTERM).
- [x] `/health` liveness endpoint; healthchecks in `server.Dockerfile` + compose.
- [x] DB migrations run at startup (`sqlx` migrate, `create_if_missing`).
- [x] Container restart policy (`restart: unless-stopped`) + `depends_on: service_healthy` in compose.
- [ ] **DB backups** — periodic snapshot of the `/data` volume (SQLite file); test restore.
- [ ] Deeper readiness check (verify DB + Suwayomi reachability, distinct from liveness).
- [ ] Graceful degradation when Suwayomi is down (reader mock fallback helps the UI; server should surface a clear partial-availability state).

## CI/CD

- [x] **CI** — `.github/workflows/ci.yml`: web typecheck+build (reader/admin/packages/worker), root lint (prettier+eslint), Rust `fmt` + `clippy -D warnings` + release build; pnpm-store and cargo caching.
- [x] **E2E smoke** job (manual / `workflow_dispatch`) running Playwright against the built reader.
- [~] **Lighthouse/CWV** budget job — stub (`workflow_dispatch`), needs real config.
- [ ] **CD** — build + push server image, deploy on tag/main; deploy reader/admin to the static host.
- [ ] Preview deploys for PRs (reader/admin).
- [ ] Supply-chain: `cargo audit` / `pnpm audit` gates; pinned action SHAs.

## Payments

- [ ] Payment provider integration (Stripe or similar) for donate/support (reader has `/donate` + `/support` routes today, UI-only).
- [ ] Webhook handling + reconciliation; entitlement/subscription state in the server.
- [ ] PCI scope kept minimal (hosted checkout; no card data on our servers).
- [ ] Receipts, refunds, tax/VAT handling.

## Legal

- [ ] Terms of Service + Privacy Policy (multi-user accounts store credentials + activity).
- [ ] Cookie/consent handling if analytics are added.
- [ ] DMCA / takedown process and clear stance on federated third-party sources (Suwayomi surfaces external content).
- [ ] Data export / deletion (account deletion, GDPR/CCPA basics).
- [ ] Content ratings / age gating where required.

## Infra

- [x] **Containerization** — `deploy/server.Dockerfile` (multi-stage, non-root, healthcheck), `deploy/reader.Dockerfile` (nginx SPA).
- [x] Local stack — `deploy/docker-compose.yml` (suwayomi + server + reader, network, volumes, healthchecks).
- [x] **DB persistence** — SQLite on a named volume (`server-data` → `/data`).
- [ ] Real deploy target chosen + provisioned (Fly.io / Railway / VPS) with a persistent volume at `/data`.
- [ ] Worker + B2 image pipeline deployed (`wrangler deploy`) and wired to the reader (`PUBLIC_KOMIKA_IMG_MODE`).
- [ ] IaC / reproducible provisioning (Fly `fly.toml`, Terraform, or Compose on a managed host).
- [ ] Staging environment separate from prod.
- [ ] Backup + disaster-recovery runbook.

---

### Nearest-term priorities

1. Finish security headers + CSP on the reader; lock `CORS_ORIGINS` to real origins.
2. Stand up TLS + a WAF/CDN in front of both the SPA and the API.
3. DB backup/restore for the `/data` SQLite volume.
4. Turn the Lighthouse stub into an enforced CWV budget.
5. Wire CD (server image publish + deploy; SPA deploy).
