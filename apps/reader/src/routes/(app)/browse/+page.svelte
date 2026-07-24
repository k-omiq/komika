<script module lang="ts">
	import type { FederatedResultView } from '$lib/data/source';

	type BrowseCacheEntry = {
		rows: FederatedResultView[];
		// There is deliberately no page number here: the page is part of the cache KEY
		// (see the rows effect's `sig`) and it lives in the URL, which SvelteKit restores
		// on a back-nav. So "Back must return to the page I was on" costs the cache
		// nothing — an entry only has to restore the rows and scroll FOR that page.
		hasNext: boolean;
		totalCount: number | null;
		rowsAreFederated: boolean;
		scrollY: number;
	};
	// Back-nav restore (B1): keep the last few browse result sets in MODULE memory —
	// it survives a component remount (series → back) but not a full reload — so
	// returning to Browse rehydrates instantly with scroll intact instead of
	// re-running the whole search. Keyed by a query+filters+PAGE signature; small FIFO
	// cap so it can't grow unbounded across a long session.
	const browseCache = new Map<string, BrowseCacheEntry>();
	// Raised from 8 when the page number joined the key: at 8, paging one query eight
	// deep evicted every other query's entry, so "page a bit, open a series, go back,
	// page again" always missed. 12 × ≤20 view objects is trivial memory.
	const BROWSE_CACHE_MAX = 12;
	function putBrowseCache(sig: string, entry: BrowseCacheEntry) {
		browseCache.delete(sig); // re-insert so it becomes most-recent
		browseCache.set(sig, entry);
		while (browseCache.size > BROWSE_CACHE_MAX) {
			const oldest = browseCache.keys().next().value as string;
			browseCache.delete(oldest);
		}
	}
</script>

<script lang="ts">
	import { page } from '$app/state';
	import { beforeNavigate, goto } from '$app/navigation';
	import { tick } from 'svelte';
	import Icon from '$lib/components/Icon.svelte';
	import MangaCard from '$lib/components/MangaCard.svelte';
	import CardGridSkeleton from '$lib/components/CardGridSkeleton.svelte';
	import Pager from '$lib/components/Pager.svelte';
	import { FLAG, STATUS_META, type ComicType, type Status } from '$lib/data/types';
	import { FEED_PAGE_SIZE, lastPage, pageParam, withPage } from '$lib/data/paging';
	import { getFederatedSearch, getNativeSearch } from '$lib/data/source';
	import { auth } from '$lib/auth.svelte';
	import { backend } from '$lib/context';

	let { data } = $props();

	// Genre facets (S4): the full genre set across the persisted catalogue, most
	// common first, driving the genre multi-select. `data.facets` never rejects.
	let facets = $state<{ genre: string; count: number }[]>([]);
	$effect(() => {
		data.facets.then((f) => (facets = f));
	});

	const params = page.url.searchParams;
	let query = $state(params.get('q') ?? '');
	let types = $state<ComicType[]>(params.get('type') ? [params.get('type') as ComicType] : []);
	let selectedGenres = $state<string[]>(params.get('genre') ? [params.get('genre')!] : []);
	let status = $state<Status | 'any'>('any');
	// Rating is a dual-handle range on the 0–10 scale (10 = no upper bound).
	let minRating = $state(0);
	let maxRating = $state(10);

	// TEMPORARILY DISABLED — re-enable once MangaDex statistics are ingested into
	// `work_stats` (see docs/plans/2026-07-23-architecture-decisions.md, AD-6).
	//
	// The server filters rating on AVG(score) FROM reviews — LOCAL user reviews —
	// and the entire database contains 3 of them. Measured against the live API:
	// no filter → 10,574 results; minRating 0.5 (one notch) → 3. The max handle is
	// inert because unrated series COALESCE to 0. So the control silently deleted
	// 99.97% of the catalogue on first touch, which is strictly worse than having
	// no control at all. Hiding it is the honest interim state; the facet returns
	// on real data (rating_bayesian), it is not being dropped.
	const RATING_FILTER_ENABLED = false;
	let sort = $state<'trending' | 'rating' | 'newest' | 'chapters'>('trending');
	let genreQuery = $state(''); // filters the (long) facet list

	const TYPES: ComicType[] = ['Manga', 'Manhwa', 'Manhua'];
	const VALID_STATUS = ['ongoing', 'completed', 'hiatus', 'cancelled'];
	const VALID_SORT = ['trending', 'rating', 'newest', 'chapters'];

	// Re-sync inputs/filters from the URL on same-route client navigation (home
	// genre links, the search overlay's advanced filters). Only reads page.url, so
	// writing this state can't feed back into the effect — user edits made via the
	// rail (which don't touch the URL) are preserved until the next navigation.
	//
	// GATED on a signature of the FILTER params only, `page` deliberately excluded.
	// The pager writes `?page=` and nothing else, and rail edits are component state
	// that is never written to the URL — so without this guard every page step
	// re-hydrated the filter block and WIPED the user's genre/status/format/sort
	// selections (and reset `federatedFor`) back to whatever the URL happened to say.
	// `page` is read separately, by `pageNum`.
	let lastUrlSig: string | null = null;
	$effect(() => {
		const sp = page.url.searchParams;
		const urlSig = JSON.stringify([
			sp.get('q') ?? '',
			sp.get('type') ?? '',
			sp.getAll('genre'),
			sp.get('status') ?? '',
			sp.get('sort') ?? '',
			sp.get('minRating') ?? '',
		]);
		// Starts null, so the first run still hydrates — mount behaviour is unchanged.
		if (urlSig === lastUrlSig) return;
		lastUrlSig = urlSig;
		query = sp.get('q') ?? '';
		// A navigation (new search term, home genre link) always restarts on the fast
		// local path; the viewer re-opts into the federated fan-out per query.
		federatedFor = null;
		const tp = sp.get('type');
		types = tp && TYPES.includes(tp as ComicType) ? [tp as ComicType] : [];
		selectedGenres = sp.getAll('genre');
		const st = sp.get('status');
		status = st && VALID_STATUS.includes(st) ? (st as Status) : 'any';
		const so = sp.get('sort');
		sort = so && VALID_SORT.includes(so) ? (so as typeof sort) : 'trending';
		// Only hydrate the rating from the URL while the facet is enabled. With it
		// disabled the control is hidden, so a stale/shared `?minRating=` link must not
		// still narrow the (signed-in) federated results or render a phantom "N.N
		// rating" pill the user can't clear from the hidden slider. See
		// RATING_FILTER_ENABLED.
		if (RATING_FILTER_ENABLED) {
			const mr = sp.get('minRating');
			const mrn = mr ? parseFloat(mr) : NaN;
			minRating = Number.isNaN(mrn) ? 0 : Math.min(10, Math.max(0, mrn));
		}
	});

	function toggle<T>(arr: T[], v: T): T[] {
		return arr.includes(v) ? arr.filter((x) => x !== v) : [...arr, v];
	}

	// The facet list filtered by the genre search box (case-insensitive), always
	// keeping already-selected genres visible.
	const shownFacets = $derived.by(() => {
		const q = genreQuery.trim().toLowerCase();
		if (!q) return facets;
		return facets.filter(
			(f) => f.genre.toLowerCase().includes(q) || selectedGenres.includes(f.genre),
		);
	});

	// ---- rows: server-filtered native / client-filtered federated -------------
	// Signed-in viewers get FEDERATED search across every installed extension
	// (deduped works tagged with translators); genre/rating filtering there is
	// client-side (searchAllSources takes no filter args) so a filter change does
	// NOT re-fan-out (protects the rate limit). Everything else — an empty query
	// (whole-catalogue browse) or a signed-out text search — goes to the public
	// NATIVE search with genre/rating applied SERVER-side, re-fetching on change.
	let rows = $state<FederatedResultView[]>([]);
	let rowsLoading = $state(true);
	let rowsError = $state(false);
	let rowsAreFederated = $state(false);
	let searchNotice = $state<string | null>(null);
	let reloadKey = $state(0);
	const queryActive = $derived(query.trim().length > 0);

	// Text search hits the local catalogue (FTS over the canonical `work` table,
	// AD-5) by DEFAULT — fast, deterministic, and returning `w_` works that carry the
	// source picker. The federated "search every installed extension" fan-out is slow
	// (up to 24 live source calls) and runs ONLY when the signed-in viewer asks for it
	// explicitly, to discover titles not yet in the catalogue.
	//
	// The opt-in is stored as THE QUERY IT WAS GRANTED FOR, not as a boolean. As a
	// boolean it was only ever cleared by the URL-sync effect above, whose sole
	// dependency is `page.url` — and typing in the search box never touches the URL.
	// So one click on "Search external sources" made every subsequent keystroke-batch
	// fan out to 24 sources until the next navigation: the server rate limit tripped,
	// and the rateLimited branch deliberately KEEPS the previous query's rows, so the
	// grid showed "naruto" results under a header reading "one piece".
	let federatedFor = $state<string | null>(null);
	const federatedRequested = $derived(federatedFor !== null && federatedFor === query.trim());
	// Drives the in-flight label + the opt-in button visibility.
	const federatedPending = $derived(queryActive && !!auth.user && federatedRequested);
	// Offer the external-source fan-out only for a signed-in viewer on a text query
	// whose local results aren't already the federated set.
	const canSearchExternal = $derived(
		queryActive && !!auth.user && !rowsAreFederated && !federatedRequested,
	);

	// Whole-catalogue pagination (native path only): the server pages the ENTIRE
	// filtered catalogue and each page REPLACES the grid. THE page number is the URL's
	// — not component state — so a reload, a shared link and the browser's Back button
	// all agree, and the rows effect re-runs simply by reading it. Genre/rating filters
	// are applied server-side across the whole catalogue; type/status/sort are refined
	// client-side over the current page only (see `pageScopedNotice`).
	const pageNum = $derived(pageParam(page.url.searchParams));
	let hasNext = $state(false);
	let totalCount = $state<number | null>(null);

	function serverFilters() {
		return {
			genres: [...selectedGenres],
			// Gated on RATING_FILTER_ENABLED so a stale/shared ?minRating= URL can't
			// still collapse the catalogue to 3 rows while the control is hidden.
			minRating: RATING_FILTER_ENABLED && minRating > 0 ? minRating : undefined,
			maxRating: RATING_FILTER_ENABLED && maxRating < 10 ? maxRating : undefined,
		};
	}

	// Signature of the currently-displayed result set, for the back-nav cache. Native
	// results depend on query + server filters (genres/rating); federated results
	// depend only on the query (filters are applied client-side), so its signature
	// deliberately omits the filters — mirroring the effect's reactive-dependency
	// split so a federated filter change neither re-fetches nor invalidates the cache.
	let lastSig: string | null = null;
	// `reloadKey` at the time `lastSig` was set, so an explicit retry still refetches
	// even though its signature is unchanged.
	let lastReloadKey = -1;
	// Consult the cache on the FIRST effect run of a fresh mount (a back-nav into
	// Browse), never on user-driven filter/query changes — plus the one extra case
	// below.
	let didInitFromCache = false;
	// `didInitFromCache` alone only covers a REMOUNT (series → back). A pager step is a
	// SAME-ROUTE navigation, so browser-Back from page 3 to page 2 keeps the component
	// alive, leaves that flag true, and would refetch — showing a skeleton and losing
	// the scroll position (SvelteKit restores scroll before the rows land).
	// `beforeNavigate` tells us the outgoing navigation was a popstate, which re-arms
	// the cache for exactly one effect run. Clicking a Pager link is a `'link'`
	// navigation (they are real `<a href>`s), so it correctly does NOT restore: it
	// fetches fresh and scrolls to the top, which is what a forward step should do.
	let restoreOnPop = false;

	$effect(() => {
		const q = query.trim();
		// Wait for `initAuth()` (a root-layout effect) to settle before fetching.
		// Without this the first run read `auth.user === null`, fetched the whole
		// catalogue as `nat:anon:0`, then `auth.user` resolved, the signature changed
		// and it fetched the WHOLE catalogue again — a skeleton flash over results
		// that were already on screen, on every cold load for a signed-in viewer.
		if (!auth.ready) return;
		const loggedIn = !!auth.user;
		const rk = reloadKey; // manual retry re-runs the fetch
		// Federated only when the viewer has explicitly opted in for THIS query
		// (federatedRequested); otherwise a text query uses the fast local FTS path.
		const isFederated = !!q && loggedIn && federatedRequested;
		// Reading the filter state HERE (synchronously) only for the native path makes
		// the effect depend on it → native re-fetches on filter change; the federated
		// branch skips these reads so it re-fetches only on query/auth change. The page
		// number rides the same trick: the native path depends on it (a page step must
		// re-fetch), the federated path must not (it has no server pager at all).
		const nativeFilters = isFederated ? null : serverFilters();
		const nativePage = isFederated ? 1 : pageNum;
		// The cache key must capture EVERY input that changes the result set across a
		// remount, or a back-nav could serve one identity's results to another. Federated
		// results are per-authenticated-user; native results are server-filtered by the
		// viewer's NSFW preference. Both the cache and auth state survive login/logout
		// (SPA state, no reload), so fold the user id + NSFW posture into the signature.
		// (Filters still stay out of the FEDERATED sig — they're applied client-side there
		// — so a federated filter change neither re-fetches nor invalidates the cache.)
		// The native sig also carries the PAGE (`p`): each page REPLACES the grid, so two
		// pages of one query are two different result sets. Without it, back-nav from a
		// series opened on page 3 would restore page 1's 20 rows under a URL still
		// reading `?page=3`. The federated key stays page-free — federated search
		// returns a single deduped page (`hasNext = false`).
		const idTag = `${auth.user?.id ?? 'anon'}:${auth.user?.showNsfw ? 1 : 0}`;
		const sig = isFederated
			? `fed:${idTag}:${q}`
			: `nat:${idTag}:${JSON.stringify({
					q,
					g: [...nativeFilters!.genres].sort(),
					mn: nativeFilters!.minRating ?? 0,
					mx: nativeFilters!.maxRating ?? 10,
					p: nativePage,
				})}`;

		// B1 back-nav restore: on a remount OR a same-route Back (see `restoreOnPop`), if
		// we cached this exact result set, hydrate it and skip the fetch entirely, then
		// restore the scroll position.
		const mayRestore = !didInitFromCache || restoreOnPop;
		didInitFromCache = true;
		restoreOnPop = false; // one-shot
		if (mayRestore) {
			const cached = browseCache.get(sig);
			if (cached) {
				rows = cached.rows;
				hasNext = cached.hasNext;
				totalCount = cached.totalCount;
				rowsAreFederated = cached.rowsAreFederated;
				rowsError = false;
				searchNotice = null;
				rowsLoading = false;
				lastSig = sig;
				lastReloadKey = rk;
				const y = cached.scrollY;
				// Wait for the grid to paint, then jump back to where we were.
				tick().then(() => requestAnimationFrame(() => window.scrollTo(0, y)));
				return;
			}
		}

		// Nothing about the result set changed — don't refetch. The effect re-runs for
		// reasons that don't affect the query (auth resolving to the same identity, the
		// federated opt-in being cleared after a rate limit), and each of those used to
		// cost a full catalogue round trip plus a skeleton flash over live results.
		if (sig === lastSig && rk === lastReloadKey && !rowsError) {
			rowsLoading = false;
			return;
		}

		rowsLoading = true;
		// Snapshot the pager envelope: the federated rate-limit branch below deliberately
		// KEEPS the previous result set on screen, so it has to put the previous pager
		// back with it — otherwise clicking "Search external sources" and getting
		// rate-limited left the catalogue rows visible but with hasNext=false (no pager)
		// and totalCount=null (the headline dropping from "10,574 series" to "20+
		// series"), because the short-circuit above returns before any refetch can
		// restore them. The page NUMBER is not snapshotted — it lives in the URL, which
		// that branch never touches.
		const prevPager = { hasNext, total: totalCount };
		hasNext = false;
		totalCount = null;
		let cancelled = false;
		const t = setTimeout(
			async () => {
				try {
					if (isFederated) {
						const outcome = await getFederatedSearch(q);
						if (cancelled) return;
						if (outcome.kind === 'ok') {
							rows = outcome.rows;
							rowsAreFederated = true;
							rowsError = false;
							searchNotice = null;
							// Federated live search returns a single deduped page (no server pager).
							hasNext = false;
							lastSig = sig;
							lastReloadKey = rk;
						} else if (outcome.kind === 'rateLimited') {
							// Keep prior results; show a transient message, not "0 results".
							searchNotice =
								outcome.retryAfter != null
									? `Too many searches — try again in ${outcome.retryAfter}s.`
									: 'Too many searches — try again in a moment.';
							// Nothing was replaced, so put the pager back where it was — the rows
							// still on screen are the previous (native) result set and they are
							// still paged. See `prevPager`.
							hasNext = prevPager.hasNext;
							totalCount = prevPager.total;
							// Release the opt-in so the "Search external sources" button comes
							// BACK — clicking it was the only way to retry, and leaving the flag
							// set made `canSearchExternal` permanently false until a navigation.
							// The effect re-run this triggers short-circuits on the unchanged
							// signature, so the prior rows and this notice both survive.
							federatedFor = null;
						} else {
							// Not authenticated / error → public native fallback (server-filtered).
							// `nativePage` is 1 on this branch by construction, and it must be:
							// the federated signature carries no page, so fetching `pageNum` here
							// would cache page N's rows under a page-free key. `requestFederated`
							// already drops the URL back to page 1 before we can get here.
							const r = await getNativeSearch(q, serverFilters(), nativePage);
							if (cancelled) return;
							rows = r.items;
							rowsAreFederated = false;
							rowsError = r.error;
							hasNext = r.hasNext;
							totalCount = r.total;
							searchNotice =
								outcome.kind === 'error'
									? 'Live search had a problem — showing catalogue results.'
									: null;
							lastSig = sig;
							lastReloadKey = rk;
						}
					} else {
						const r = await getNativeSearch(q, nativeFilters!, nativePage);
						if (cancelled) return;
						rows = r.items;
						rowsAreFederated = false;
						rowsError = r.error;
						hasNext = r.hasNext;
						totalCount = r.total;
						searchNotice = null;
						lastSig = sig;
						lastReloadKey = rk;
					}
				} finally {
					if (!cancelled) rowsLoading = false;
				}
			},
			q ? 280 : 160,
		);
		return () => {
			cancelled = true;
			clearTimeout(t);
		};
	});

	// Snapshot the current result set + scroll into the module cache whenever we
	// navigate away (into a series, or onto another page of results), so a back-nav can
	// restore it. Keyed by the signature of what's actually on screen — which now
	// includes the page number, so each page gets its own entry.
	beforeNavigate((nav) => {
		// Skip while a fetch is in flight: a new run resets hasNext/totalCount
		// synchronously but leaves lastSig + rows on the PREVIOUS result set until the
		// fetch lands, so snapshotting now would cache a no-pager view of a multi-page
		// result. During loading the grid shows a skeleton anyway, so there's nothing on
		// screen worth preserving — a back-nav simply re-runs the search.
		if (lastSig != null && !rowsLoading && typeof window !== 'undefined') {
			putBrowseCache(lastSig, {
				rows,
				hasNext,
				totalCount,
				rowsAreFederated,
				scrollY: window.scrollY,
			});
		}
		// Re-arm the cache for the next effect run when (and only when) this navigation
		// is the browser's Back/Forward. See `restoreOnPop`.
		restoreOnPop = nav.type === 'popstate';
	});

	/** Rewrite `?page=` in place, without adding a history entry. */
	function replacePage(p: number) {
		if (p === pageNum) return;
		void goto(withPage(page.url, p), { replaceState: true, noScroll: true, keepFocus: true });
	}

	// A shared, edited or stale link can name a page past the end (the server ECHOES the
	// requested page rather than clamping — same behaviour the admin review queue relies
	// on), which would render an empty grid reading "No matches found" over a catalogue
	// of 10,000+. Land on the last real page instead. replaceState, so Back doesn't
	// bounce the viewer straight onto the bad page again.
	$effect(() => {
		if (rowsLoading || rowsError || rowsAreFederated) return;
		if (rows.length > 0 || pageNum === 1) return;
		if (totalCount == null || totalCount === 0) return;
		const last = lastPage(totalCount, FEED_PAGE_SIZE);
		if (pageNum > last) replacePage(last);
	});

	// A new query or a new SERVER-side filter is a new result set — page 7 of the old
	// one is meaningless, so drop back to page 1. replaceState so filter fiddling
	// doesn't fill the history stack.
	//
	// `types`/`status`/`sort` are deliberately ABSENT: they are applied client-side over
	// the current page, so changing them must not throw away the page the viewer is on.
	// (If they ever become server arguments they move in here.) `reloadKey` is absent
	// too — an explicit Retry must re-fetch the page the viewer is ON, not page 1.
	//
	// The viewer identity + NSFW posture ARE included: the server filters by the
	// persisted `show_nsfw` preference, so flipping the rail's toggle (or logging in)
	// yields a different result set of a different length. Gated on `auth.ready`
	// because `initAuth()` resolves the user asynchronously — recording the baseline
	// before then would see anon→user as a filter change and blow away the page number
	// from a shared `?page=3` link on every cold load for a signed-in viewer.
	let lastFilterSig: string | null = null;
	$effect(() => {
		if (!auth.ready) return;
		const fsig = JSON.stringify([
			query.trim(),
			[...selectedGenres].sort(),
			minRating,
			maxRating,
			auth.user?.id ?? 'anon',
			auth.user?.showNsfw ? 1 : 0,
		]);
		// Don't fight the initial URL: the first run only records the baseline.
		if (lastFilterSig === null) {
			lastFilterSig = fsig;
			return;
		}
		if (fsig === lastFilterSig) return;
		lastFilterSig = fsig;
		replacePage(1);
	});

	function retryRows() {
		reloadKey++;
	}

	// Opt into the federated multi-extension fan-out for the current query (signed-in
	// only). Flips the dispatch flag; the rows effect re-runs and swaps in federated
	// results (which carry per-source translator chips).
	function requestFederated() {
		if (!auth.user || federatedRequested) return;
		federatedFor = query.trim();
		// Federated results are one deduped page with no server pager, so a lingering
		// `?page=5` would describe a pager that isn't rendered — and would still be in
		// the URL when the viewer goes back to the native path.
		replacePage(1);
	}

	// NSFW visibility: the SERVER filters browse/search results by the viewer's
	// persisted `show_nsfw` preference, so the toggle flips that preference (via
	// `setShowNsfw`, shared with the profile setting) and re-fetches. Only meaningful
	// for signed-in viewers — anonymous browsing is always safe-filtered.
	const showNsfw = $derived(auth.user?.showNsfw ?? false);
	let savingNsfw = $state(false);
	async function toggleNsfw() {
		if (!auth.user || savingNsfw || !backend.setShowNsfw) return;
		savingNsfw = true;
		try {
			const next = await backend.setShowNsfw(!auth.user.showNsfw);
			if (auth.user) auth.user.showNsfw = next;
			reloadKey++; // re-fetch results under the new NSFW posture
		} catch {
			// best-effort — leave the toggle as it was
		} finally {
			savingNsfw = false;
		}
	}

	// Client-side facets the server search doesn't cover (type, status), plus
	// genre/rating for FEDERATED rows (native rows are already server-filtered).
	const results = $derived.by(() => {
		const gsel = selectedGenres.map((g) => g.toLowerCase());
		const list = rows.filter((m) => {
			if (types.length && !types.includes(m.type)) return false;
			if (status !== 'any' && m.status !== status) return false;
			if (rowsAreFederated) {
				if (gsel.length && !m.genres.some((mg) => gsel.includes(mg.toLowerCase()))) return false;
				if (minRating > 0 && m.rating < minRating) return false;
				if (maxRating < 10 && m.rating > maxRating) return false;
			}
			return true;
		});
		const sorters = {
			trending: (a: FederatedResultView, b: FederatedResultView) =>
				b.rating - a.rating || b.ch - a.ch,
			rating: (a: FederatedResultView, b: FederatedResultView) => b.rating - a.rating,
			newest: (a: FederatedResultView, b: FederatedResultView) => b.addedAt - a.addedAt,
			chapters: (a: FederatedResultView, b: FederatedResultView) => b.ch - a.ch,
		};
		return [...list].sort(sorters[sort]);
	});

	// True when a facet the SERVER doesn't apply has narrowed the view. `totalCount`
	// is a catalogue-wide, pre-pagination count, so it is only an honest headline
	// while type/status aren't filtering the loaded page down further.
	const clientNarrowed = $derived(types.length > 0 || status !== 'any');

	// HONESTY: the sort chips and the Format/Status chips are applied CLIENT-SIDE, over
	// `rows` — and `rows` is now exactly the 20 series of the current page, because the
	// server pages the catalogue and there is no server argument for either facet
	// (browse order is fixed `latest_chapter_at DESC, s.id DESC`, and comic type is
	// derived at read time with no column to filter on). Under "Load more" that was
	// merely coarse and converged as the viewer loaded more rows. Under pagination it
	// would LOOK like sorting/filtering the whole catalogue while touching 20 of 10,000+
	// rows, so the scope is stated in the UI wherever a pager is on screen. Fixing it
	// properly needs server-side `sort`/`status` args (and a materialized comic type) —
	// see the follow-up note in the PR. `pageScoped` is defined below, next to
	// `resultsLoading`, which it depends on.

	// The header used to read `${results.length}${hasNext ? '+' : ''}` — i.e. the
	// size of the current 20-row page — which is where the reported "20+ series"
	// came from. The real catalogue total was already fetched and plumbed into
	// `totalCount`; it was just never rendered here. This is the CATALOGUE-WIDE match
	// count, deliberately not the page's — the per-page range lives in the Pager.
	// Falls back to the old approximate form when the total is genuinely unknown. That is
	// NOT the text-search path — both branches of the server's `search` resolver return
	// `total: Some(..)` (the FTS branch counts in SQL, same as the browse branch), so a
	// null total means the backend is off, the request failed, or an adapter that doesn't
	// report one is in use. The federated fan-out has no pager at all and never gets here.
	const countLabel = $derived(
		totalCount != null && !clientNarrowed
			? `${totalCount.toLocaleString()} series`
			: `${results.length}${hasNext ? '+' : ''} series`,
	);

	const resultsLoading = $derived(rowsLoading);
	const catalogError = $derived(rowsError);

	// True exactly when a pager is on screen, i.e. when the client-side Sort and
	// Format/Status facets are operating on one page of a larger set. See the honesty
	// note above `clientNarrowed`.
	const pageScoped = $derived(!rowsAreFederated && !resultsLoading && (pageNum > 1 || hasNext));
	const pageScopeNote = $derived(
		`Sort and Format/Status apply to the ${rows.length} series on this page, not to all ${
			totalCount != null ? totalCount.toLocaleString() : 'matching'
		} series.`,
	);

	const anyFilter = $derived(
		types.length > 0 ||
			selectedGenres.length > 0 ||
			status !== 'any' ||
			minRating > 0 ||
			maxRating < 10,
	);
	const ratingLabel = $derived(
		minRating === 0 && maxRating >= 10
			? 'Any'
			: `${minRating.toFixed(1)} – ${maxRating >= 10 ? '10' : maxRating.toFixed(1)}`,
	);

	// `kind` exists purely to key the {#each}: the labels come from different facets
	// and are NOT mutually unique — a genre literally named "Ongoing"/"Completed"/
	// "Hiatus"/"Cancelled" would collide with the status pill, and Svelte 5 throws
	// `each_key_duplicate` in production, killing the page.
	const activePills = $derived.by(() => {
		const pills: { kind: string; label: string; remove: () => void }[] = [];
		types.forEach((t) =>
			pills.push({
				kind: 'type',
				label: `${FLAG[t]}  ${t}`,
				remove: () => (types = toggle(types, t)),
			}),
		);
		selectedGenres.forEach((g) =>
			pills.push({
				kind: 'genre',
				label: g,
				remove: () => (selectedGenres = toggle(selectedGenres, g)),
			}),
		);
		if (status !== 'any')
			pills.push({
				kind: 'status',
				label: STATUS_META[status].label,
				remove: () => (status = 'any'),
			});
		if (minRating > 0 || maxRating < 10)
			pills.push({
				kind: 'rating',
				label: `${ratingLabel} rating`,
				remove: () => {
					minRating = 0;
					maxRating = 10;
				},
			});
		return pills;
	});

	// Dual-handle rating slider: keep min ≤ max as either thumb is dragged.
	function onMinRating() {
		if (minRating > maxRating) minRating = maxRating;
	}
	function onMaxRating() {
		if (maxRating < minRating) maxRating = minRating;
	}

	const sortChips = [
		{ key: 'trending', label: 'Trending' },
		{ key: 'rating', label: 'Top rated' },
		{ key: 'newest', label: 'Newest' },
		{ key: 'chapters', label: 'Most ch.' },
	] as const;

	// Mobile: the filter rail collapses into a bottom sheet toggled by this flag.
	let filtersOpen = $state(false);

	function resetFilters() {
		types = [];
		selectedGenres = [];
		status = 'any';
		minRating = 0;
		maxRating = 10;
	}
	function resetAll() {
		query = '';
		resetFilters();
	}
</script>

<div class="head k-gutter">
	<div class="head-inner">
		<h1>Browse</h1>
		<div class="searchbar">
			<Icon name="search" size={20} stroke="#87857f" />
			<input bind:value={query} placeholder="Search series, authors, genres…" />
			{#if query}
				<button class="clear" aria-label="Clear" onclick={() => (query = '')}
					><Icon name="x" size={18} /></button
				>
			{/if}
		</div>
	</div>
</div>

<div class="body k-gutter">
	{#if filtersOpen}
		<button class="filter-scrim" aria-label="Close filters" onclick={() => (filtersOpen = false)}
		></button>
	{/if}
	<aside class="rail" class:open={filtersOpen}>
		<div class="rail-head">
			<span class="rail-title">Filters</span>
			<div class="rail-head-right">
				{#if anyFilter}<button class="reset" onclick={resetFilters}>Reset</button>{/if}
				<button class="sheet-done" onclick={() => (filtersOpen = false)}>Done</button>
			</div>
		</div>

		<div class="group">
			<span class="glabel">Format</span>
			<div class="chips">
				{#each TYPES as t (t)}
					<button
						class="chip"
						class:on={types.includes(t)}
						onclick={() => (types = toggle(types, t))}
					>
						<span class="flag">{FLAG[t]}</span>{t}
					</button>
				{/each}
			</div>
		</div>

		<div class="group">
			<div class="glabel-row">
				<span class="glabel">Genre</span>
				{#if selectedGenres.length}<span class="glabel-count">{selectedGenres.length} selected</span
					>{/if}
			</div>
			<div class="genre-search">
				<Icon name="search" size={15} stroke="#87857f" />
				<input bind:value={genreQuery} placeholder="Filter genres…" aria-label="Filter genres" />
				{#if genreQuery}
					<button class="gs-clear" aria-label="Clear" onclick={() => (genreQuery = '')}
						><Icon name="x" size={14} /></button
					>
				{/if}
			</div>
			<div class="genre-list">
				{#if facets.length === 0}
					<span class="genre-empty">Genres load with the catalogue…</span>
				{:else}
					{#each shownFacets as f (f.genre)}
						<button
							class="genre-opt"
							class:on={selectedGenres.includes(f.genre)}
							onclick={() => (selectedGenres = toggle(selectedGenres, f.genre))}
						>
							<span class="go-check" aria-hidden="true">
								{#if selectedGenres.includes(f.genre)}<Icon
										name="check"
										size={12}
										strokeWidth={2.6}
									/>{/if}
							</span>
							<span class="go-name">{f.genre}</span>
							<span class="go-count">{f.count}</span>
						</button>
					{:else}
						<span class="genre-empty">No genres match “{genreQuery}”.</span>
					{/each}
				{/if}
			</div>
		</div>

		<div class="group">
			<span class="glabel">Status</span>
			<div class="chips">
				{#each ['any', 'ongoing', 'completed', 'hiatus', 'cancelled'] as s (s)}
					<button
						class="chip"
						class:on={status === s}
						onclick={() => (status = s as Status | 'any')}
					>
						{s === 'any' ? 'Any' : STATUS_META[s as Status].label}
					</button>
				{/each}
			</div>
		</div>

		<div class="group">
			<span class="glabel">Content</span>
			<div class="nsfw-row">
				<div class="nsfw-text">
					<span class="nsfw-label">Show NSFW</span>
					<span class="nsfw-desc">
						{auth.user ? 'Include adult-rated series' : 'Sign in to include adult-rated series'}
					</span>
				</div>
				<button
					type="button"
					class="switch"
					class:on={showNsfw}
					role="switch"
					aria-checked={showNsfw}
					aria-label="Show NSFW content"
					disabled={savingNsfw || !auth.user}
					onclick={toggleNsfw}
				>
					<span class="knob"></span>
				</button>
			</div>
		</div>

		<!-- Hidden until ratings come from MangaDex statistics rather than the 3-row
		     local `reviews` table — see RATING_FILTER_ENABLED above. -->
		{#if RATING_FILTER_ENABLED}
			<div class="group">
				<div class="rating-head">
					<span class="glabel">Rating</span>
					<span class="rating-val">
						{#if minRating > 0 || maxRating < 10}<Icon
								name="star"
								size={12}
								fill="var(--k-star)"
							/>{/if}{ratingLabel}
					</span>
				</div>
				<div class="range" style="--min:{(minRating / 10) * 100}%;--max:{(maxRating / 10) * 100}%">
					<div class="range-rail"></div>
					<div class="range-fill"></div>
					<input
						class="range-input"
						type="range"
						min="0"
						max="10"
						step="0.5"
						bind:value={minRating}
						oninput={onMinRating}
						aria-label="Minimum rating"
					/>
					<input
						class="range-input"
						type="range"
						min="0"
						max="10"
						step="0.5"
						bind:value={maxRating}
						oninput={onMaxRating}
						aria-label="Maximum rating"
					/>
				</div>
				<div class="rating-scale"><span>0</span><span>10</span></div>
			</div>
		{/if}
	</aside>

	<div class="results" id="browse-results" aria-busy={resultsLoading}>
		<button class="filter-trigger" onclick={() => (filtersOpen = true)}>
			<Icon name="sliders-h" size={16} />Filters{#if activePills.length}<span class="ft-badge"
					>{activePills.length}</span
				>{/if}
		</button>
		<div class="results-head">
			<div class="results-title">
				<span class="rt">{query.trim() ? `"${query.trim()}"` : 'All series'}</span>
				<span class="rc"
					>{resultsLoading
						? queryActive
							? federatedPending
								? 'Searching all sources…'
								: 'Searching…'
							: 'Loading…'
						: countLabel}</span
				>
			</div>
			<div class="sort">
				<span class="sort-label">{pageScoped ? 'Sort this page' : 'Sort'}</span>
				<div class="chips">
					{#each sortChips as so (so.key)}
						<button class="sortchip" class:on={sort === so.key} onclick={() => (sort = so.key)}
							>{so.label}</button
						>
					{/each}
				</div>
			</div>
		</div>

		<!-- The client-side scope of Sort / Format / Status, stated plainly whenever a
		     pager is on screen. Without this the controls read as catalogue-wide while
		     they only touch the 20 rows of the current page. -->
		{#if pageScoped}
			<p class="scope-note">{pageScopeNote}</p>
		{/if}

		{#if activePills.length}
			<div class="pills">
				{#each activePills as p (`${p.kind}:${p.label}`)}
					<button class="pill" onclick={p.remove}
						>{p.label}<Icon name="x" size={13} stroke="#9c9a94" strokeWidth={2.4} /></button
					>
				{/each}
			</div>
		{/if}

		{#if searchNotice}
			<div class="notice" role="status">
				<Icon name="alert" size={16} />
				<span>{searchNotice}</span>
			</div>
		{/if}

		{#if resultsLoading}
			<CardGridSkeleton count={12} />
		{:else if results.length}
			<div class="grid">
				{#each results as m (m.id ?? m.title)}
					<MangaCard
						title={m.title}
						sub={`${m.genre} · ${m.ch} ch`}
						rating={m.rating.toFixed(1)}
						cover={m.cover}
						id={m.id}
						flagType={m.type}
						status={{ label: STATUS_META[m.status].label, color: STATUS_META[m.status].color }}
						translators={m.translators ?? []}
					/>
				{/each}
			</div>
			{#if canSearchExternal}
				<div class="ext-search">
					<span class="ext-hint">Not finding it in the catalogue?</span>
					<button class="ext-btn" onclick={requestFederated}>
						<Icon name="search" size={15} />Search external sources
					</button>
				</div>
			{/if}
		{:else if !queryActive && catalogError}
			<div class="empty">
				<div class="empty-icon error"><Icon name="alert" size={24} /></div>
				<div class="empty-title">Couldn't load the catalogue</div>
				<div class="empty-desc">
					Something went wrong reaching the server. Check your connection and try again.
				</div>
				<button class="empty-btn" onclick={retryRows}>Retry</button>
			</div>
		{:else if clientNarrowed && rows.length > 0}
			<!-- The page HAS results; the client-side Format/Status chips just matched none
			     of them. The generic "No matches found" below would claim the catalogue is
			     empty, which is a lie — and this branch keeps the pager rendered so the
			     viewer can page onward to where the matches actually are. -->
			<div class="empty">
				<div class="empty-icon"><Icon name="search" size={24} /></div>
				<div class="empty-title">Nothing on this page matches</div>
				<div class="empty-desc">
					The Format and Status filters apply to the {rows.length} series on this page. Try another page,
					or clear them to see the whole page.
				</div>
				<button class="empty-btn" onclick={resetFilters}>Clear filters</button>
			</div>
		{:else if !searchNotice}
			<div class="empty">
				<div class="empty-icon"><Icon name="search" size={24} /></div>
				<div class="empty-title">No matches found</div>
				<div class="empty-desc">
					{canSearchExternal
						? 'Nothing in the catalogue matches. Try searching external sources.'
						: 'Try a different search term or loosen your filters.'}
				</div>
				{#if canSearchExternal}
					<button class="empty-btn" onclick={requestFederated}>Search external sources</button>
				{:else}
					<button class="empty-btn" onclick={resetAll}>Clear all</button>
				{/if}
			</div>
		{/if}

		<!-- Native path only: the federated fan-out returns a single deduped page with no
		     server pager. `Pager` hides itself on page 1 with no next page, so a result
		     set that fits in one page renders no pager at all. Past page 1 it stays on
		     screen through a load (its links become inert aria-disabled spans) so the
		     landmark and live region survive, rather than vanishing under the click that
		     triggered the load. `aria-controls` is deliberately not passed: the only
		     candidate region is `.results`, which CONTAINS this pager. -->
		{#if !rowsAreFederated}
			<Pager
				page={pageNum}
				{hasNext}
				total={totalCount}
				pageSize={FEED_PAGE_SIZE}
				count={rows.length}
				loading={resultsLoading}
				href={(p) => withPage(page.url, p)}
				label="Browse results pages"
			/>
		{/if}
	</div>
</div>

<style>
	.head {
		padding-top: 44px;
	}
	.head-inner {
		max-width: 960px;
	}
	h1 {
		font-size: 34px;
		margin: 0 0 20px;
		color: var(--k-text-bright);
	}
	.searchbar {
		display: flex;
		align-items: center;
		gap: 14px;
		height: 56px;
		padding: 0 10px 0 22px;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
		border-radius: 12px;
		transition: border-color 0.15s;
	}
	.searchbar:focus-within {
		border-color: var(--k-border-strong);
	}
	.searchbar input {
		flex: 1;
		min-width: 0;
		background: transparent;
		border: none;
		outline: none;
		color: var(--k-text);
		font-size: 16px;
	}
	.clear {
		width: 36px;
		height: 36px;
		border: none;
		background: transparent;
		color: var(--k-text-faint);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.clear:hover {
		color: var(--k-text);
	}
	.flag {
		font-size: 17px;
		line-height: 1;
	}
	.body {
		display: grid;
		grid-template-columns: 236px 1fr;
		gap: 44px;
		padding-top: 32px;
		padding-bottom: 80px;
		align-items: start;
	}
	.rail {
		display: flex;
		flex-direction: column;
		gap: 30px;
		position: sticky;
		top: 88px;
	}
	.rail-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.rail-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 16px;
		color: var(--k-text);
	}
	.reset {
		background: none;
		border: none;
		font-size: 12.5px;
		font-weight: 700;
		color: var(--k-text-dimmer);
		cursor: pointer;
	}
	.reset:hover {
		color: var(--k-text);
	}
	.group {
		display: flex;
		flex-direction: column;
		gap: 12px;
	}
	.glabel {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.09em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.nsfw-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 14px;
	}
	.nsfw-text {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
	}
	.nsfw-label {
		font-size: 13.5px;
		font-weight: 600;
		color: var(--k-text-1);
	}
	.nsfw-desc {
		font-size: 12px;
		color: var(--k-text-faint);
		line-height: 1.35;
	}
	.switch {
		flex: 0 0 auto;
		width: 44px;
		height: 26px;
		border-radius: 999px;
		border: 1px solid var(--k-border-4);
		background: var(--k-border-1);
		padding: 0;
		cursor: pointer;
		position: relative;
		transition:
			background 0.15s,
			border-color 0.15s;
	}
	.switch:disabled {
		opacity: 0.6;
		cursor: default;
	}
	.switch.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
	}
	.switch .knob {
		position: absolute;
		top: 2px;
		left: 2px;
		width: 20px;
		height: 20px;
		border-radius: 50%;
		background: var(--k-on-primary, #fff);
		transition: transform 0.15s;
	}
	.switch.on .knob {
		transform: translateX(18px);
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		font-size: 12.5px;
		font-weight: 600;
		padding: 6px 13px;
		border-radius: var(--k-radius-pill);
		cursor: pointer;
		background: var(--k-hover-fill);
		border: 1px solid var(--k-border-3);
		color: var(--k-text-3);
		transition: all 0.15s;
	}
	.chip.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.rating-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.rating-val {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 13px;
		font-weight: 700;
		color: var(--k-text);
	}
	/* genre multi-select (S4) */
	.glabel-row {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 8px;
	}
	.glabel-count {
		font-size: 11px;
		font-weight: 700;
		color: var(--k-primary);
	}
	.genre-search {
		display: flex;
		align-items: center;
		gap: 8px;
		height: 36px;
		padding: 0 8px 0 12px;
		border-radius: 9px;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
	}
	.genre-search:focus-within {
		border-color: var(--k-border-strong);
	}
	.genre-search input {
		flex: 1;
		min-width: 0;
		background: transparent;
		border: none;
		outline: none;
		color: var(--k-text);
		font-size: 13px;
	}
	.gs-clear {
		width: 26px;
		height: 26px;
		border: none;
		background: transparent;
		color: var(--k-text-faint);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.genre-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		max-height: 232px;
		overflow-y: auto;
		padding-right: 2px;
	}
	.genre-opt {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 7px 9px;
		border-radius: 8px;
		border: 1px solid transparent;
		background: transparent;
		color: var(--k-text-3);
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
		text-align: left;
		transition: background 0.12s;
	}
	.genre-opt:hover {
		background: var(--k-hover-fill);
	}
	.genre-opt.on {
		background: rgba(224, 131, 105, 0.12);
		border-color: rgba(224, 131, 105, 0.4);
		color: var(--k-text-bright);
	}
	.go-check {
		width: 18px;
		height: 18px;
		flex: 0 0 auto;
		border-radius: 5px;
		border: 1px solid var(--k-border-3);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: var(--k-on-primary);
	}
	.genre-opt.on .go-check {
		background: var(--k-primary);
		border-color: var(--k-primary);
	}
	.go-name {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.go-count {
		flex: 0 0 auto;
		font-size: 11.5px;
		font-weight: 700;
		color: var(--k-text-faint);
	}
	.genre-empty {
		font-size: 12.5px;
		color: var(--k-text-faint);
		padding: 6px 2px;
	}
	/* dual-handle rating range slider */
	.range {
		position: relative;
		height: 24px;
		--min: 0%;
		--max: 100%;
	}
	.range-rail,
	.range-fill {
		position: absolute;
		top: 50%;
		height: 4px;
		border-radius: 4px;
		transform: translateY(-50%);
		pointer-events: none;
	}
	.range-rail {
		left: 0;
		right: 0;
		background: var(--k-border-4);
	}
	.range-fill {
		left: var(--min);
		right: calc(100% - var(--max));
		background: var(--k-star);
	}
	/* Two native range inputs stacked; only the thumbs receive pointer events so
	   both handles stay grabbable. */
	.range-input {
		position: absolute;
		inset: 0;
		width: 100%;
		margin: 0;
		background: transparent;
		-webkit-appearance: none;
		appearance: none;
		pointer-events: none;
	}
	.range-input::-webkit-slider-thumb {
		-webkit-appearance: none;
		appearance: none;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		background: var(--k-text-bright);
		border: 2px solid var(--k-star);
		cursor: pointer;
		pointer-events: auto;
	}
	.range-input::-moz-range-thumb {
		width: 18px;
		height: 18px;
		border-radius: 50%;
		background: var(--k-text-bright);
		border: 2px solid var(--k-star);
		cursor: pointer;
		pointer-events: auto;
	}
	.rating-scale {
		display: flex;
		justify-content: space-between;
		font-size: 10.5px;
		color: var(--k-text-disabled);
	}
	.results {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.results-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
		flex-wrap: wrap;
	}
	.results-title {
		display: flex;
		align-items: baseline;
		gap: 11px;
	}
	.rt {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 18px;
		color: var(--k-text);
	}
	.rc {
		font-size: 13.5px;
		color: var(--k-text-faint);
	}
	.sort {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.sort-label {
		font-size: 12.5px;
		color: var(--k-text-faint);
	}
	.sortchip {
		font-size: 12.5px;
		font-weight: 700;
		padding: 7px 13px;
		border-radius: var(--k-radius-pill);
		cursor: pointer;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-dimmer);
		transition: all 0.15s;
	}
	.sortchip.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.pills {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.pill {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		font-size: 12px;
		font-weight: 600;
		padding: 6px 8px 6px 12px;
		border-radius: var(--k-radius-pill);
		background: rgba(255, 255, 255, 0.07);
		border: 1px solid var(--k-border-2);
		color: var(--k-text-1);
		cursor: pointer;
	}
	.notice {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 16px;
		border-radius: 10px;
		background: rgba(246, 183, 60, 0.1);
		border: 1px solid rgba(246, 183, 60, 0.34);
		color: var(--k-text-2);
		font-size: 13.5px;
		font-weight: 600;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(158px, 1fr));
		gap: 30px 22px;
	}
	/* Scope disclosure for the client-side Sort / Format / Status facets. Sits
	   directly under the results head, above the grid, so it is read before the
	   cards rather than discovered after them. */
	.scope-note {
		margin: -10px 0 0;
		font-size: 12.5px;
		line-height: 1.45;
		color: var(--k-text-faint);
	}
	.ext-search {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
		padding: 22px 0 4px;
	}
	.ext-hint {
		font-size: 12.5px;
		color: var(--k-text-faint);
	}
	.ext-btn {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		height: 40px;
		padding: 0 20px;
		border-radius: var(--k-radius-pill);
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
		color: var(--k-text);
		font-weight: 700;
		font-size: 13px;
		cursor: pointer;
		transition: border-color 0.15s;
	}
	.ext-btn:hover {
		border-color: var(--k-border-strong);
	}
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 14px;
		padding: 80px 20px;
		text-align: center;
	}
	.empty-icon {
		width: 56px;
		height: 56px;
		border-radius: 50%;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-1);
		display: flex;
		align-items: center;
		justify-content: center;
		color: var(--k-text-disabled);
	}
	.empty-icon.error {
		color: var(--k-hiatus);
		border-color: rgba(246, 183, 60, 0.4);
	}
	.empty-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 18px;
		color: var(--k-text);
	}
	.empty-desc {
		font-size: 14px;
		color: var(--k-text-dimmer);
		max-width: 320px;
		line-height: 1.5;
	}
	.empty-btn {
		margin-top: 6px;
		height: 42px;
		padding: 0 22px;
		border-radius: 8px;
		background: var(--k-primary);
		border: none;
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 13.5px;
		cursor: pointer;
	}
	/* Mobile filter sheet controls — hidden on desktop/tablet. */
	.rail-head-right {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.sheet-done {
		display: none;
	}
	.filter-trigger {
		display: none;
	}
	.filter-scrim {
		display: none;
	}
	@media (max-width: 820px) {
		.body {
			grid-template-columns: 1fr;
			gap: 28px;
		}
		.rail {
			position: static;
		}
	}
	@media (max-width: 640px) {
		/* Rail becomes a bottom sheet; results take the full width with a Filters
		   trigger. Declared after the 820 block so it wins the shared props. */
		.filter-trigger {
			display: inline-flex;
			align-items: center;
			gap: 8px;
			align-self: flex-start;
			height: 40px;
			padding: 0 16px;
			margin-bottom: 4px;
			border-radius: var(--k-radius-pill);
			background: var(--k-surface-2);
			border: 1px solid var(--k-border-3);
			color: var(--k-text);
			font-size: 13.5px;
			font-weight: 700;
			cursor: pointer;
		}
		.ft-badge {
			display: inline-flex;
			align-items: center;
			justify-content: center;
			min-width: 18px;
			height: 18px;
			padding: 0 5px;
			border-radius: 9px;
			background: var(--k-primary);
			color: var(--k-on-primary);
			font-size: 11px;
			font-weight: 800;
		}
		.filter-scrim {
			display: block;
			position: fixed;
			inset: 0;
			z-index: 59;
			border: none;
			background: rgba(8, 8, 9, 0.55);
			backdrop-filter: blur(2px);
		}
		.rail {
			position: fixed;
			left: 0;
			right: 0;
			bottom: 0;
			top: auto;
			z-index: 60;
			max-height: 84vh;
			overflow-y: auto;
			gap: 22px;
			padding: 18px 20px calc(24px + env(safe-area-inset-bottom));
			background: var(--k-surface-3);
			border: 1px solid var(--k-border-2);
			border-radius: 18px 18px 0 0;
			box-shadow: 0 -20px 60px rgba(0, 0, 0, 0.5);
			transform: translateY(110%);
			transition: transform 0.28s cubic-bezier(0.4, 0, 0.2, 1);
		}
		.rail.open {
			transform: translateY(0);
		}
		.sheet-done {
			display: inline-flex;
			align-items: center;
			height: 32px;
			padding: 0 16px;
			border-radius: var(--k-radius-pill);
			background: var(--k-primary);
			border: none;
			color: var(--k-on-primary);
			font-size: 13px;
			font-weight: 700;
			cursor: pointer;
		}
		.grid {
			grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
			gap: 22px 14px;
		}
	}
</style>
