import { AwsClient } from 'aws4fetch';

/**
 * Komika image Worker.
 *
 * The web build can't cross-origin-fetch arbitrary source CDNs (CORS), so this
 * Worker fetches image bytes server-side and re-serves them with permissive
 * CORS + long cache headers. It also read-through/write-through caches popular
 * or user-marked images into Backblaze B2 (S3-compatible).
 *
 * URL scheme (must match packages/api/src/image-provider.ts):
 *
 *   GET /img?src=<url-encoded upstream image URL>[&cache=1]
 *
 *   - src   : absolute http(s) URL of the upstream image (required).
 *   - cache : optional write-through hint. When "1", a B2 miss that is served
 *             from upstream is also persisted to B2. Cache-fill jobs / the
 *             scanner pass this for popular / marked series; normal reads omit
 *             it so the long tail is only edge-cached, never stored in B2.
 *
 * Caching layers, in order:
 *   1. Cloudflare edge cache (caches.default) — always on.
 *   2. B2 read-through — if B2 is configured, try B2 before upstream.
 *   3. Upstream fetch — origin of last resort. On &cache=1, write-through to B2.
 *
 * Everything B2-related fails open: any B2 error falls back to a plain upstream
 * proxy, so a broken bucket never breaks reads.
 */

interface Env {
	ALLOWED_SOURCE_HOSTS?: string;
	ALLOWED_ORIGINS?: string;
	B2_ENDPOINT?: string;
	B2_REGION?: string;
	B2_BUCKET?: string;
	// Secrets (wrangler secret put):
	B2_KEY_ID?: string;
	B2_APP_KEY?: string;
}

const IMMUTABLE_CACHE = 'public, max-age=604800, immutable';
const CORS_HEADERS: Record<string, string> = {
	'Access-Control-Allow-Origin': '*',
	'Access-Control-Allow-Methods': 'GET, HEAD, OPTIONS',
	'Access-Control-Allow-Headers': '*',
	'Access-Control-Max-Age': '86400',
};

export default {
	async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
		// CORS preflight.
		if (request.method === 'OPTIONS') {
			return new Response(null, { status: 204, headers: CORS_HEADERS });
		}

		if (request.method !== 'GET' && request.method !== 'HEAD') {
			return errorResponse(405, 'Method Not Allowed');
		}

		const url = new URL(request.url);
		if (url.pathname !== '/img') {
			// Trivial health check / anything else.
			if (url.pathname === '/' || url.pathname === '/healthz') {
				return new Response('ok', { status: 200, headers: CORS_HEADERS });
			}
			return errorResponse(404, 'Not Found');
		}

		// --- Validate `src`. -----------------------------------------------------
		const src = url.searchParams.get('src');
		if (!src) return errorResponse(400, 'Missing ?src');

		let upstream: URL;
		try {
			upstream = new URL(src);
		} catch {
			return errorResponse(400, 'Invalid ?src URL');
		}
		if (upstream.protocol !== 'http:' && upstream.protocol !== 'https:') {
			return errorResponse(400, 'src must be http(s)');
		}
		if (!hostAllowed(upstream.hostname, env.ALLOWED_SOURCE_HOSTS)) {
			return errorResponse(400, 'src host not allowed');
		}

		// --- Hotlink protection. -------------------------------------------------
		if (!originAllowed(request, env.ALLOWED_ORIGINS)) {
			return errorResponse(403, 'Forbidden origin');
		}

		const wantsCache = url.searchParams.get('cache') === '1';

		// --- 1. Edge cache. ------------------------------------------------------
		// Key on the normalized upstream URL so /img?src=X and /img?src=X&cache=1
		// (and any hotlink query noise) share one edge entry.
		const cacheKey = new Request(
			`${url.origin}/img?src=${encodeURIComponent(upstream.toString())}`,
			{ method: 'GET' },
		);
		const edge = caches.default;
		const cached = await edge.match(cacheKey);
		if (cached) return withMethod(cached, request.method);

		// --- 2. B2 read-through. -------------------------------------------------
		const b2 = getB2(env);
		const objectKey = await b2Key(upstream.toString());

		if (b2) {
			const hit = await b2Get(b2, objectKey).catch(() => null);
			if (hit && hit.ok && hit.body) {
				const res = finalizeImage(hit, hit.headers.get('Content-Type'));
				ctx.waitUntil(edge.put(cacheKey, res.clone()));
				return withMethod(res, request.method);
			}
		}

		// --- 3. Upstream fetch. --------------------------------------------------
		let originResp: Response;
		try {
			originResp = await fetch(upstream.toString(), {
				headers: {
					// Some CDNs require a Referer / UA to serve images; be polite.
					Referer: `${upstream.protocol}//${upstream.host}/`,
					'User-Agent': 'KomikaImageWorker/1.0 (+https://komika.app)',
					Accept: 'image/*,*/*;q=0.8',
				},
				redirect: 'follow',
			});
		} catch {
			return errorResponse(502, 'Upstream fetch failed');
		}
		if (!originResp.ok || !originResp.body) {
			return errorResponse(502, `Upstream ${originResp.status}`);
		}

		const contentType = originResp.headers.get('Content-Type') ?? 'application/octet-stream';

		// Write-through to B2 only when hinted, and only if B2 is configured.
		// We must tee the body so we can both serve the client and store to B2
		// without buffering the whole image ourselves.
		if (b2 && wantsCache) {
			const [toClient, toB2] = originResp.body.tee();
			// Persist in the background so the client isn't blocked on the B2 PUT.
			ctx.waitUntil(
				b2Put(b2, objectKey, toB2, contentType, originResp.headers.get('Content-Length')).catch(
					() => undefined,
				),
			);
			const res = finalizeImage(new Response(toClient), contentType);
			ctx.waitUntil(edge.put(cacheKey, res.clone()));
			return withMethod(res, request.method);
		}

		const res = finalizeImage(originResp, contentType);
		ctx.waitUntil(edge.put(cacheKey, res.clone()));
		return withMethod(res, request.method);
	},
} satisfies ExportedHandler<Env>;

// --- helpers ---------------------------------------------------------------

/** Build the response we serve + cache: image bytes, CORS, long cache. */
function finalizeImage(source: Response, contentType: string | null): Response {
	const headers = new Headers();
	headers.set('Content-Type', contentType || 'application/octet-stream');
	headers.set('Cache-Control', IMMUTABLE_CACHE);
	for (const [k, v] of Object.entries(CORS_HEADERS)) headers.set(k, v);
	// Preserve length when known; streaming responses may omit it.
	const len = source.headers.get('Content-Length');
	if (len) headers.set('Content-Length', len);
	return new Response(source.body, { status: 200, headers });
}

/** HEAD requests get headers only. */
function withMethod(res: Response, method: string): Response {
	if (method === 'HEAD') {
		return new Response(null, { status: res.status, headers: res.headers });
	}
	return res;
}

function errorResponse(status: number, message: string): Response {
	return new Response(message, {
		status,
		headers: { 'Content-Type': 'text/plain; charset=utf-8', ...CORS_HEADERS },
	});
}

/** Parse a comma-separated allowlist var into a trimmed, non-empty list. */
function parseList(raw: string | undefined): string[] {
	if (!raw) return [];
	return raw
		.split(',')
		.map((s) => s.trim())
		.filter(Boolean);
}

/** Empty allowlist => allow all. Otherwise match host exactly or as a suffix. */
function hostAllowed(hostname: string, raw: string | undefined): boolean {
	const list = parseList(raw);
	if (list.length === 0) return true;
	const host = hostname.toLowerCase();
	return list.some((allowed) => {
		const a = allowed.toLowerCase();
		return host === a || host.endsWith(`.${a}`);
	});
}

/**
 * Hotlink protection. Requests with no Origin and no Referer (native apps,
 * curl, cache-fill jobs, direct hits) are always allowed. Browser requests are
 * checked against the allowlist; an empty allowlist disables the check.
 */
function originAllowed(request: Request, raw: string | undefined): boolean {
	const list = parseList(raw);
	if (list.length === 0) return true;

	const origin = request.headers.get('Origin');
	const referer = request.headers.get('Referer');
	if (!origin && !referer) return true; // non-browser client

	const candidate = origin ?? originOf(referer);
	if (!candidate) return true;
	return list.includes(candidate);
}

function originOf(referer: string | null): string | null {
	if (!referer) return null;
	try {
		return new URL(referer).origin;
	} catch {
		return null;
	}
}

// --- B2 (S3-compatible) ----------------------------------------------------

interface B2Config {
	client: AwsClient;
	endpoint: string; // no trailing slash
	bucket: string;
}

function getB2(env: Env): B2Config | null {
	if (!env.B2_ENDPOINT || !env.B2_BUCKET || !env.B2_KEY_ID || !env.B2_APP_KEY) {
		return null;
	}
	const client = new AwsClient({
		accessKeyId: env.B2_KEY_ID,
		secretAccessKey: env.B2_APP_KEY,
		service: 's3',
		region: env.B2_REGION || 'us-east-1',
	});
	return {
		client,
		endpoint: env.B2_ENDPOINT.replace(/\/$/, ''),
		bucket: env.B2_BUCKET,
	};
}

/** Stable object key from the upstream URL: sha-256 hex, sharded 2/2. */
async function b2Key(upstreamUrl: string): Promise<string> {
	const data = new TextEncoder().encode(upstreamUrl);
	const digest = await crypto.subtle.digest('SHA-256', data);
	const hex = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
	return `img/${hex.slice(0, 2)}/${hex.slice(2, 4)}/${hex}`;
}

function b2Url(b2: B2Config, key: string): string {
	return `${b2.endpoint}/${b2.bucket}/${key}`;
}

function b2Get(b2: B2Config, key: string): Promise<Response> {
	return b2.client.fetch(b2Url(b2, key), { method: 'GET' });
}

function b2Put(
	b2: B2Config,
	key: string,
	body: ReadableStream,
	contentType: string,
	contentLength: string | null,
): Promise<Response> {
	const headers: Record<string, string> = {
		'Content-Type': contentType,
		'Cache-Control': IMMUTABLE_CACHE,
	};
	// S3 PUT with a streaming body requires a known length; without it, aws4fetch
	// can't sign correctly, so only stream-PUT when the upstream gave a length.
	if (contentLength) headers['Content-Length'] = contentLength;
	return b2.client.fetch(b2Url(b2, key), {
		method: 'PUT',
		headers,
		body,
	});
}
