# @komika/worker — image proxy + B2 cache

A Cloudflare Worker that fetches upstream manga image bytes server-side and
re-serves them with permissive CORS and long cache headers. The web reader can't
cross-origin-fetch arbitrary source CDNs (CORS), so it points its `<img src>` at
this Worker instead. Popular / user-marked images are additionally cached in
Backblaze B2; the long tail is just proxied and edge-cached.

## URL scheme

```
GET /img?src=<url-encoded upstream image URL>[&cache=1]
```

- **`src`** (required) — absolute `http(s)` URL of the upstream image. Rejected
  with `400` if missing, not `http(s)`, or (when `ALLOWED_SOURCE_HOSTS` is set)
  not in the source-host allowlist.
- **`cache=1`** (optional) — write-through hint. On a B2 miss served from
  upstream, also persist the object to B2. Cache-fill jobs / the scanner pass
  this for popular / marked series; normal reads omit it, so ordinary traffic is
  only edge-cached, never written to B2. The Worker itself doesn't know
  popularity — the caller decides by setting this flag.

Also: `GET /` and `GET /healthz` return `ok`. `OPTIONS` is handled as a CORS
preflight.

## Caching layers

1. **Cloudflare edge** (`caches.default`) — always on. Keyed on the normalized
   upstream URL.
2. **B2 read-through** — if B2 is configured, tried before upstream.
3. **Upstream** — origin of last resort. On `&cache=1`, written through to B2.

All B2 operations **fail open**: any B2 error falls back to a plain upstream
proxy, so a broken/offline bucket never breaks reads.

## Run locally

```sh
pnpm --filter @komika/worker dev     # wrangler dev on http://localhost:8787
```

Type-check without deploying:

```sh
pnpm --filter @komika/worker check   # tsc --noEmit
```

Deploy:

```sh
pnpm --filter @komika/worker deploy  # wrangler deploy
```

## Configuration

Non-secret config lives in `wrangler.toml` under `[vars]`:

| var                    | meaning                                                                                                                                                                     |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ALLOWED_SOURCE_HOSTS` | Comma-separated upstream host allowlist (prevents open-proxy abuse). Empty = allow all. Matches host exactly or as a suffix. For MangaDex, include `uploads.mangadex.org` (covers) **and** `mangadex.network` (the `*.mangadex.network` MangaDex@Home page hosts — the suffix entry covers every node). |
| `ALLOWED_ORIGINS`      | Comma-separated browser Origin/Referer allowlist (hotlink protection). Empty = disabled. Requests with no Origin/Referer are always allowed (native apps, cache-fill jobs). |
| `B2_ENDPOINT`          | B2 S3-compatible endpoint, e.g. `https://s3.us-west-004.backblazeb2.com`. Empty = B2 disabled.                                                                              |
| `B2_REGION`            | Region matching the endpoint, e.g. `us-west-004`.                                                                                                                           |
| `B2_BUCKET`            | B2 bucket name.                                                                                                                                                             |

### Secrets

Set with `wrangler secret put` (never in `wrangler.toml`):

```sh
wrangler secret put B2_KEY_ID    # Backblaze applicationKeyId
wrangler secret put B2_APP_KEY   # Backblaze applicationKey
```

For local `wrangler dev`, copy `.dev.vars.example` to `.dev.vars` and fill in
`B2_KEY_ID` / `B2_APP_KEY`. `.dev.vars` is gitignored.

B2 is fully optional — with `B2_BUCKET` empty (or secrets unset) the Worker runs
as a pure proxy + edge cache.

## Pointing the reader at it

The reader reads two `PUBLIC_*` env vars (see `apps/reader/src/lib/config.ts`):

```sh
PUBLIC_KOMIKA_IMG_MODE=proxy                    # (default) route through this Worker
PUBLIC_KOMIKA_IMG_WORKER=http://localhost:8787  # Worker base URL (this default)
```

In proxy mode the reader builds `${PUBLIC_KOMIKA_IMG_WORKER}/img?src=<encoded>`,
which matches this Worker's scheme exactly. Set `PUBLIC_KOMIKA_IMG_MODE=direct`
to bypass the Worker and use source URLs unchanged (for already-CORS-safe hosts
like a Suwayomi server that proxies images itself).

## B2 setup

1. Create a Backblaze B2 bucket.
2. Create an application key scoped to that bucket; note the `keyID` and
   `applicationKey`.
3. Find your bucket's S3 endpoint (Backblaze bucket details → "S3 Endpoint"),
   e.g. `s3.us-west-004.backblazeb2.com`, and its region (`us-west-004`).
4. Put `B2_ENDPOINT` (`https://` + the endpoint), `B2_REGION`, `B2_BUCKET` in
   `wrangler.toml`; set the two secrets as above.

Objects are keyed as `img/<aa>/<bb>/<sha256-of-upstream-url>` (2/2 sharded) with
`Content-Type` and an immutable `Cache-Control` preserved.
