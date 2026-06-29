<script lang="ts">
	import { page } from '$app/state';
	import Icon from '$lib/components/Icon.svelte';
	import MangaCard from '$lib/components/MangaCard.svelte';
	import CardGridSkeleton from '$lib/components/CardGridSkeleton.svelte';
	import { FLAG, STATUS_META, type ComicType, type Status } from '$lib/data/types';
	import {
		getFederatedSearch,
		getNativeSearch,
		type FederatedResultView,
	} from '$lib/data/source';
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
	let sort = $state<'trending' | 'rating' | 'newest' | 'chapters'>('trending');
	let genreQuery = $state(''); // filters the (long) facet list

	const TYPES: ComicType[] = ['Manga', 'Manhwa', 'Manhua'];
	const VALID_STATUS = ['ongoing', 'completed', 'hiatus', 'cancelled'];
	const VALID_SORT = ['trending', 'rating', 'newest', 'chapters'];

	// Re-sync inputs/filters from the URL on same-route client navigation (home
	// genre links, the search overlay's advanced filters). Only reads page.url, so
	// writing this state can't feed back into the effect — user edits made via the
	// rail (which don't touch the URL) are preserved until the next navigation.
	$effect(() => {
		const sp = page.url.searchParams;
		query = sp.get('q') ?? '';
		const tp = sp.get('type');
		types = tp && TYPES.includes(tp as ComicType) ? [tp as ComicType] : [];
		selectedGenres = sp.getAll('genre');
		const st = sp.get('status');
		status = st && VALID_STATUS.includes(st) ? (st as Status) : 'any';
		const so = sp.get('sort');
		sort = so && VALID_SORT.includes(so) ? (so as typeof sort) : 'trending';
		const mr = sp.get('minRating');
		const mrn = mr ? parseFloat(mr) : NaN;
		minRating = Number.isNaN(mrn) ? 0 : Math.min(10, Math.max(0, mrn));
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

	// Whole-catalogue pagination (native path only): the server pages the ENTIRE
	// filtered catalogue, so "Load more" pulls the next page and appends rather than
	// capping Browse at the first 20. Genre/rating filters are applied server-side
	// across the whole catalogue; type/status are refined client-side over everything
	// loaded so far, so paging through extends what those filters can match.
	let catalogPage = $state(1);
	let hasNext = $state(false);
	let totalCount = $state<number | null>(null);
	let loadingMore = $state(false);

	function serverFilters() {
		return {
			genres: [...selectedGenres],
			minRating: minRating > 0 ? minRating : undefined,
			maxRating: maxRating < 10 ? maxRating : undefined,
		};
	}

	$effect(() => {
		const q = query.trim();
		const loggedIn = !!auth.user;
		reloadKey; // manual retry re-runs the fetch
		const isFederated = !!q && loggedIn;
		// Reading the filter state HERE (synchronously) only for the native path makes
		// the effect depend on it → native re-fetches on filter change; the federated
		// branch skips these reads so it re-fetches only on query/auth change.
		const nativeFilters = isFederated ? null : serverFilters();
		rowsLoading = true;
		// A fresh query/filter run always restarts paging from page 1.
		catalogPage = 1;
		hasNext = false;
		totalCount = null;
		let cancelled = false;
		const t = setTimeout(async () => {
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
					} else if (outcome.kind === 'rateLimited') {
						// Keep prior results; show a transient message, not "0 results".
						searchNotice =
							outcome.retryAfter != null
								? `Too many searches — try again in ${outcome.retryAfter}s.`
								: 'Too many searches — try again in a moment.';
					} else {
						// Not authenticated / error → public native fallback (server-filtered).
						const r = await getNativeSearch(q, serverFilters());
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
					}
				} else {
					const r = await getNativeSearch(q, nativeFilters!);
					if (cancelled) return;
					rows = r.items;
					rowsAreFederated = false;
					rowsError = r.error;
					hasNext = r.hasNext;
					totalCount = r.total;
					searchNotice = null;
				}
			} finally {
				if (!cancelled) rowsLoading = false;
			}
		}, q ? 280 : 160);
		return () => {
			cancelled = true;
			clearTimeout(t);
		};
	});

	function retryRows() {
		reloadKey++;
	}

	// Pull the next catalogue page and append it. Native path only — the federated
	// live search has no server pager. Dedupes by id so an offset shift between pages
	// (concurrent catalogue writes) can't produce a duplicate `{#each}` key.
	async function loadMore() {
		if (loadingMore || !hasNext || rowsAreFederated) return;
		loadingMore = true;
		const next = catalogPage + 1;
		try {
			const r = await getNativeSearch(query.trim(), serverFilters(), next);
			const seen = new Set(rows.map((m) => m.id).filter(Boolean));
			rows = [...rows, ...r.items.filter((m) => !m.id || !seen.has(m.id))];
			catalogPage = next;
			hasNext = r.hasNext;
			totalCount = r.total;
		} finally {
			loadingMore = false;
		}
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

	const resultsLoading = $derived(rowsLoading);
	const catalogError = $derived(rowsError);

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

	const activePills = $derived.by(() => {
		const pills: { label: string; remove: () => void }[] = [];
		types.forEach((t) =>
			pills.push({ label: `${FLAG[t]}  ${t}`, remove: () => (types = toggle(types, t)) }),
		);
		selectedGenres.forEach((g) =>
			pills.push({ label: g, remove: () => (selectedGenres = toggle(selectedGenres, g)) }),
		);
		if (status !== 'any')
			pills.push({ label: STATUS_META[status].label, remove: () => (status = 'any') });
		if (minRating > 0 || maxRating < 10)
			pills.push({
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
						{auth.user
							? 'Include adult-rated series'
							: 'Sign in to include adult-rated series'}
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
	</aside>

	<div class="results">
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
							? 'Searching all sources…'
							: 'Loading…'
						: `${results.length}${hasNext ? '+' : ''} series`}</span
				>
			</div>
			<div class="sort">
				<span class="sort-label">Sort</span>
				<div class="chips">
					{#each sortChips as so (so.key)}
						<button class="sortchip" class:on={sort === so.key} onclick={() => (sort = so.key)}
							>{so.label}</button
						>
					{/each}
				</div>
			</div>
		</div>

		{#if activePills.length}
			<div class="pills">
				{#each activePills as p (p.label)}
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
		{:else if !queryActive && catalogError}
			<div class="empty">
				<div class="empty-icon error"><Icon name="alert" size={24} /></div>
				<div class="empty-title">Couldn't load the catalogue</div>
				<div class="empty-desc">
					Something went wrong reaching the server. Check your connection and try again.
				</div>
				<button class="empty-btn" onclick={retryRows}>Retry</button>
			</div>
		{:else if !searchNotice}
			<div class="empty">
				<div class="empty-icon"><Icon name="search" size={24} /></div>
				<div class="empty-title">No matches found</div>
				<div class="empty-desc">Try a different search term or loosen your filters.</div>
				<button class="empty-btn" onclick={resetAll}>Clear all</button>
			</div>
		{/if}

		{#if !resultsLoading && hasNext && !rowsAreFederated}
			<div class="load-more">
				<button class="load-more-btn" disabled={loadingMore} onclick={loadMore}>
					{loadingMore ? 'Loading…' : 'Load more'}
				</button>
				{#if totalCount != null}
					<span class="load-more-count">Showing {rows.length} of {totalCount}</span>
				{/if}
			</div>
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
	.load-more {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 10px;
		padding: 8px 0 4px;
	}
	.load-more-btn {
		height: 44px;
		padding: 0 28px;
		border-radius: var(--k-radius-pill);
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
		color: var(--k-text);
		font-weight: 700;
		font-size: 13.5px;
		cursor: pointer;
		transition: all 0.15s;
	}
	.load-more-btn:hover:not(:disabled) {
		border-color: var(--k-border-strong);
	}
	.load-more-btn:disabled {
		opacity: 0.6;
		cursor: default;
	}
	.load-more-count {
		font-size: 12.5px;
		color: var(--k-text-faint);
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
