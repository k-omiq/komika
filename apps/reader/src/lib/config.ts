import { env } from '$env/dynamic/public';

/**
 * Runtime configuration. Values are public by nature (this is a client app) and
 * come from PUBLIC_* env vars, with dev-friendly defaults:
 *  - The Komika GraphQL API defaults to the local server on 8080 (matches the
 *    default `backendKind: 'komika'` below and the admin app's default).
 *  - The image Worker defaults to a local `wrangler dev` on 8787.
 */
export const config = {
	apiEndpoint: env.PUBLIC_KOMIKA_API ?? 'http://localhost:8080/graphql',
	imgWorkerBaseUrl: env.PUBLIC_KOMIKA_IMG_WORKER ?? 'http://localhost:8787',
	/**
	 * Whether screens pull from the live backend. Off by default so the app runs
	 * standalone against `mock.ts`. Set PUBLIC_KOMIKA_BACKEND=on once a backend is
	 * available; the repository still falls back to mock on error.
	 */
	backendEnabled: env.PUBLIC_KOMIKA_BACKEND === 'on',
	/**
	 * Which backend implementation to use:
	 *  - 'komika'   → the unified Komika GraphQL API (default, to-spec).
	 *  - 'suwayomi' → a direct Suwayomi/Tachidesk adapter (real catalog now, no social).
	 */
	backendKind: (env.PUBLIC_KOMIKA_BACKEND_KIND ?? 'komika') as 'komika' | 'suwayomi',
	/** Suwayomi base URL when backendKind === 'suwayomi'. */
	suwayomiUrl: env.PUBLIC_SUWAYOMI_URL ?? 'http://localhost:4567',
	/**
	 * Image mode: 'direct' returns source URLs unchanged (for already-CORS-safe
	 * hosts like Suwayomi); 'proxy' (default) routes through the Cloudflare Worker.
	 */
	imgDirect: env.PUBLIC_KOMIKA_IMG_MODE === 'direct',
};
