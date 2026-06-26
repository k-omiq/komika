# Komiq — Go-Live Guide

Step-by-step to finish deploying Komiq to production (`komiq.cc`) from the current
state. Split by **machine**: the Tunnel *must* run on the VPS; the Workers are
easiest to deploy from your Mac (working `pnpm` + `wrangler login`).

- **Cloudflare Account ID:** `c016ecc2b0cf125f1977b70ae8b0b51b` (from your R2 endpoint;
  use if wrangler asks which account).
- Legend: 🖥️ = run on the **VPS** (SSH) · 💻 = run on your **Mac** (repo root).
- See also `deploy/CLOUDFLARE-RUNBOOK.md` (Parts A–F) for the reference deploy.

## Already done (don't redo)
- ✅ VPS stack live: `komika-server` (`:8080`) + Suwayomi, both healthy, restart-on-boot.
- ✅ Admin account (`admin` / `<admin-password>`), 4 seeded series, Litestream→R2 backups running.
- ✅ CORS already allows `komiq.cc`, `www.komiq.cc`, `admin.komiq.cc`.
- ✅ `cloudflared` installed on the VPS.
- ✅ Worker configs ready: `apps/worker` (img, hotlink-locked), `apps/reader`, `apps/admin`.

## What's left
1. Get the code onto GitHub/Mac (Step 0)
2. Bring up the **Tunnel** → `api.komiq.cc` (VPS)
3. Deploy **3 Workers** + custom domains (Mac)
4. Verify end-to-end

**DNS is automatic:** `cloudflared tunnel route dns` creates the `api` record, and each
Worker "Custom Domain" creates its own. All proxied (orange cloud). No manual records.

---

## Step 0 — Get the code to your Mac 🖥️→💻

The admin `wrangler.toml` + this guide live in a VPS commit that isn't pushed yet.

**On the VPS** (create a GitHub token at github.com → Settings → Developer settings →
Personal access tokens, `repo` scope):
```bash
cd ~/komiq/komika
git push "https://<YOUR_GH_TOKEN>@github.com/gecallidryas/komika.git" main
# alternative: gh auth login  &&  git push origin main
```

**On your Mac:**
```bash
cd /Users/caved/dev/komika
git pull origin main
pnpm install
```

---

## Step 1 — Cloudflare Tunnel → `api.komiq.cc` 🖥️ (VPS)

Puts the API on the internet. Outbound-only, so Oracle's firewall is irrelevant.

```bash
# 1. Authenticate (opens a URL — open in any browser, pick komiq.cc)
cloudflared tunnel login

# 2. Create the tunnel; note the UUID it prints
cloudflared tunnel create komiq

# 3. Auto-create the proxied DNS record for the API
cloudflared tunnel route dns komiq api.komiq.cc
```

Write the config (replace `<UUID>` with the value from step 2):
```bash
cat > ~/.cloudflared/config.yml <<'YAML'
tunnel: <UUID>
credentials-file: /home/ubuntu/.cloudflared/<UUID>.json
ingress:
  - hostname: api.komiq.cc
    service: http://localhost:8080
  - service: http_status:404
YAML
```

Install as a service and verify:
```bash
sudo cloudflared service install
sudo systemctl enable --now cloudflared
sudo systemctl status cloudflared --no-pager     # active (running)

curl -fsS https://api.komiq.cc/health             # → ok
```
✅ **Gate:** do not proceed until `https://api.komiq.cc/health` returns `ok`.
The reader and admin apps are useless until this works.

---

## Step 2 — Image Worker → `img.komiq.cc` 💻 (Mac)

Pure proxy, no build step. `ALLOWED_ORIGINS` is already set to your real origins.
```bash
cd /Users/caved/dev/komika/apps/worker
wrangler login          # if not already
wrangler deploy
```
Dashboard: **Workers & Pages → komika-img → Settings → Domains & Routes → Add Custom
Domain → `img.komiq.cc`**.

Verify: `curl -I https://img.komiq.cc/` (a 400/403 on a bare hit is fine — the Worker
is answering).

---

## Step 3 — Reader Worker → `komiq.cc` 💻 (Mac)

Backend URLs are **baked in at build time** (Vite inlines them via `$env/static/public`
so they work at edge module scope — a runtime Worker `[vars]` value never reaches the
frozen module constants). The prod values live in the **committed** `apps/reader/.env.production`
— you do **not** pass them inline. Just build and deploy:
```bash
cd /Users/caved/dev/komika/apps/reader
pnpm build          # loads .env + .env.production → api.komiq.cc, backend on, img proxy
wrangler deploy
```
To change an endpoint, edit `apps/reader/.env.production` (committed) and rebuild — do
not rely on inline `PUBLIC_* … pnpm build`, which the committed `.env` files would win over.

Dashboard: **komika-reader → Add Custom Domain → `komiq.cc`**, then again for **`www.komiq.cc`**.

Verify (public catalog is edge-SSR — real series titles must be in the HTML, not just skeletons):
```bash
curl -fsS https://komiq.cc/ | grep -o '<title>.*</title>'
curl -fsS "https://komiq.cc/?cb=$RANDOM" | grep -oE 'href="/series/[^"]+"' | head   # non-empty
```

---

## Step 4 — Admin Worker → `admin.komiq.cc` 💻 (Mac)

Static SPA; only `PUBLIC_KOMIKA_API` is baked in. CORS already configured server-side.
```bash
cd /Users/caved/dev/komika/apps/admin
PUBLIC_KOMIKA_API=https://api.komiq.cc/graphql pnpm build     # emits build/
wrangler deploy
```
Dashboard: **komika-admin → Add Custom Domain → `admin.komiq.cc`**.

Verify: `curl -I https://admin.komiq.cc/` → `200`.

---

## Step 5 — Full end-to-end check (browser)

| Check | How | Pass = |
|---|---|---|
| API live | `curl https://api.komiq.cc/health` | `ok` |
| Reader home | open `https://komiq.cc` | series rows load |
| Read a chapter | open a series → read | pages load via `img.komiq.cc` |
| Social | register a user → post a comment | succeeds |
| Admin console | open `https://admin.komiq.cc`, log in `admin`/`<admin-password>` | dashboard loads |
| SPA deep link | at admin, go to `/sources` and **refresh** | no 404 (fallback works) |
| Admin ↔ API (CORS) | in admin, load series list / run an action | returns data |

---

## Optional — lock down the admin console 🔒

`admin.komiq.cc` is a management surface. Beyond the admin login, add **Cloudflare
Access**: Dashboard → Zero Trust → Access → Applications → Add → Self-hosted, domain
`admin.komiq.cc`, policy = allow only your email. The login page is then unreachable
without passing Access first.

---

## Troubleshooting

| Symptom | Cause / Fix |
|---|---|
| `api.komiq.cc/health` fails | Tunnel not connected → `sudo systemctl status cloudflared`; check `~/.cloudflared/config.yml` UUID + credentials-file path |
| Reader loads but images broken | `img.komiq.cc` custom domain not added, or origin not in the img Worker's `ALLOWED_ORIGINS` (`komiq.cc`,`www.komiq.cc`) |
| Admin calls fail with CORS error | Origin not in server `CORS_ORIGINS`; to add one, edit `deploy/.env` then `cd deploy && docker compose up -d server` |
| Admin deep-link 404 on refresh | `not_found_handling="single-page-application"` missing — ensure you deployed after Step 0's pull |
| wrangler picks wrong account | `export CLOUDFLARE_ACCOUNT_ID=c016ecc2b0cf125f1977b70ae8b0b51b` before `wrangler deploy` |
| Reader shows localhost API / empty rows | `apps/reader/.env.production` missing or wrong — it must carry `PUBLIC_KOMIKA_API=https://api.komiq.cc/graphql` + `PUBLIC_KOMIKA_BACKEND=on`; rebuild + redeploy (Step 3) |
| Reader home is skeletons in `curl` (but fine in browser) | Edge SSR must render cards, not stream them — confirm `apps/reader/src/routes/(app)/+page.ts` awaits `getHome()` on the server; then a plain `curl https://komiq.cc/` contains `/series/…` links |

## URLs are baked at build time
The reader bakes its `PUBLIC_*` config via `$env/static/public` from the committed
`apps/reader/.env` (dev) + `apps/reader/.env.production` (prod); the admin inlines
`PUBLIC_KOMIKA_API` the same way. This is deliberate: adapter-cloudflare evaluates the
reader's config at edge cold-start with no request context, so a runtime `$env/dynamic/public`
(or Worker `[vars]`) value is empty there and the app would freeze to localhost + render
empty feeds. If the API domain ever changes, edit `.env.production` then **rebuild +
redeploy** both (Steps 3 & 4). Only the Tunnel and CORS are runtime.
