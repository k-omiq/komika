import type { PageLoad } from './$types';
import { getHome } from '$lib/data/source';
import { SSR_PUBLIC } from '$lib/ssr';

// Public landing — server-rendered at the edge on the web build, client-only in
// the Tauri/static build. See $lib/ssr.
export const ssr = SSR_PUBLIC;

// Stream the home feeds: return the promise (unawaited) so the page can render
// skeleton placeholders via {#await} while the backend resolves. `getHome()`
// never rejects — it resolves to empty feeds on error.
export const load: PageLoad = ({ setHeaders }) => {
	// Edge-cache the anonymous render briefly: fresh within a minute, but the
	// backend is hit at most ~once/minute per edge rather than once per visitor.
	// No-op in the browser (setHeaders only affects the SSR response).
	setHeaders({ 'cache-control': 'public, max-age=0, s-maxage=60' });
	return { home: getHome() };
};
