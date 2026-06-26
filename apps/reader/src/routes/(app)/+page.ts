import { browser } from '$app/environment';
import type { PageLoad } from './$types';
import { getHome } from '$lib/data/source';
import { SSR_PUBLIC } from '$lib/ssr';

// Public landing — server-rendered at the edge on the web build, client-only in
// the Tauri/static build. See $lib/ssr.
export const ssr = SSR_PUBLIC;

// Home feeds. `getHome()` never rejects — it resolves to empty feeds on error.
//
// On the SERVER we AWAIT the feeds and hand the page resolved data: Svelte's SSR
// only ever renders the pending branch of `{#await}`, so a streamed promise would
// leave the edge HTML as skeletons forever (no server-rendered content, no SEO).
// Awaiting means the edge renders real cards — absorbed by the s-maxage edge cache
// so the backend is still hit at most ~once/minute per edge.
//
// In the BROWSER (client-side navigations, and the whole Tauri/static build) we
// return the promise UNawaited so the page shows skeleton placeholders via
// `{#await}` while the backend resolves. The page renders both shapes (see
// +page.svelte): a resolved object on the server/hydration, a promise on the client.
export const load: PageLoad = async ({ setHeaders }) => {
	setHeaders({ 'cache-control': 'public, max-age=0, s-maxage=60' });
	const home = getHome();
	return { home: browser ? home : await home };
};
