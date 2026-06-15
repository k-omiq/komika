import type { PageLoad } from './$types';
import { getUpdates } from '$lib/data/source';
import { SSR_PUBLIC } from '$lib/ssr';

// Public updates feed — server-rendered at the edge on the web build. See $lib/ssr.
export const ssr = SSR_PUBLIC;

// Stream the updates feeds so the page can show skeletons while they resolve.
// `getUpdates()` never rejects — it resolves to empty feeds on error.
export const load: PageLoad = ({ setHeaders }) => {
	// Shortest TTL of the public pages — this is the "what's new" surface.
	setHeaders({ 'cache-control': 'public, max-age=0, s-maxage=30' });
	return { updates: getUpdates() };
};
