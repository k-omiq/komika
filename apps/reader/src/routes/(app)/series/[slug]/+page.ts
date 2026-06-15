import type { PageLoad } from './$types';
import { getSeries } from '$lib/data/source';
import { SSR_PUBLIC } from '$lib/ssr';

// Public series detail — server-rendered at the edge on the web build (SEO +
// shareable link previews). Per-viewer bits (library mark, progress) hydrate
// client-side. See $lib/ssr.
export const ssr = SSR_PUBLIC;

// Stream the series detail so the page can show a hero/chapter skeleton while
// it resolves. `getSeries()` never rejects — it resolves to null on error and
// the page renders its not-found state.
export const load: PageLoad = ({ params, setHeaders }) => {
	setHeaders({ 'cache-control': 'public, max-age=0, s-maxage=60' });
	return {
		slug: params.slug,
		series: getSeries(params.slug),
	};
};
