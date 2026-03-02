/**
 * Komika image Worker.
 *
 * The web build can't cross-origin-fetch arbitrary source CDNs (CORS), so this
 * Worker fetches image bytes server-side and re-serves them with permissive
 * CORS + long cache headers. Popular images stay hot in Cloudflare's edge cache
 * (caches.default); there is no object-storage tier — the Worker is a pure
 * proxy backed by the edge cache.
 *
 * URL scheme (must match packages/api/src/image-provider.ts):
 *
 *   GET /img?src=<url-encoded upstream image URL>
 *
 *   - src : absolute http(s) URL of the upstream image (required). The host must
 *           be in ALLOWED_SOURCE_HOSTS; an empty allowlist denies all (fail closed).
 *
 * Caching layers, in order:
 *   1. Cloudflare edge cache (caches.default) — always on.
 *   2. Upstream fetch — origin of last resort; the response is cached at the edge.
 */

interface Env {
	ALLOWED_SOURCE_HOSTS?: string;
	ALLOWED_ORIGINS?: string;
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

		// --- 1. Edge cache. ------------------------------------------------------
		// Key on the normalized upstream URL so /img?src=X and any hotlink query
		// noise share one edge entry.
		const cacheKey = new Request(
			`${url.origin}/img?src=${encodeURIComponent(upstream.toString())}`,
			{ method: 'GET' },
		);
		const edge = caches.default;
		const cached = await edge.match(cacheKey);
		if (cached) return withMethod(cached, request.method);

		// --- 2. Upstream fetch. --------------------------------------------------
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
		} catch (err) {
			console.error(`img proxy: upstream fetch failed for ${upstream.toString()}:`, err);
			return errorResponse(502, 'Upstream fetch failed');
		}
		if (!originResp.ok || !originResp.body) {
			console.error(`img proxy: upstream ${originResp.status} for ${upstream.toString()}`);
			return errorResponse(502, `Upstream ${originResp.status}`);
		}

		const contentType = originResp.headers.get('Content-Type') ?? 'application/octet-stream';

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

/**
 * Empty allowlist => deny all (fail closed) — an unconfigured proxy must never be
 * an open proxy (I1/CR5). Otherwise match host exactly or as a suffix.
 */
function hostAllowed(hostname: string, raw: string | undefined): boolean {
	const list = parseList(raw);
	if (list.length === 0) return false;
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
