import type { PageLoad } from './$types';
import { getSeries } from '$lib/data/source';

// Stream the series detail so the page can show a hero/chapter skeleton while
// it resolves. `getSeries()` never rejects — it falls back to mock on error.
export const load: PageLoad = ({ params }) => ({
	slug: params.slug,
	series: getSeries(params.slug),
});
