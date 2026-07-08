<script lang="ts">
	import { page } from '$app/state';
	import Icon from '$lib/components/Icon.svelte';
	import MangaCard from '$lib/components/MangaCard.svelte';
	import Footer from '$lib/components/Footer.svelte';
	import CardGridSkeleton from '$lib/components/CardGridSkeleton.svelte';
	import {
		ALL_GENRES,
		FLAG,
		STATUS_META,
		type CatalogEntry,
		type ComicType,
		type Status,
	} from '$lib/data/mock';

	let { data } = $props();
	// Filters stay interactive while the catalog streams in; the results area
	// shows a skeleton until it resolves. `data.catalog` never rejects.
	let catalog = $state<CatalogEntry[]>([]);
	let loading = $state(true);
	$effect(() => {
		loading = true;
		data.catalog.then((list) => {
			catalog = list;
			loading = false;
		});
	});

	const params = page.url.searchParams;
	let query = $state(params.get('q') ?? '');
	let types = $state<ComicType[]>(params.get('type') ? [params.get('type') as ComicType] : []);
	let genres = $state<string[]>(params.get('genre') ? [params.get('genre')!] : []);
	let status = $state<Status | 'any'>('any');
	let minRating = $state(0);
	let sort = $state<'trending' | 'rating' | 'newest' | 'chapters'>('trending');

	const TYPES: ComicType[] = ['Manga', 'Manhwa', 'Manhua'];

	function toggle<T>(arr: T[], v: T): T[] {
		return arr.includes(v) ? arr.filter((x) => x !== v) : [...arr, v];
	}

	const results = $derived.by(() => {
		const q = query.trim().toLowerCase();
		let list = catalog.filter((m) => {
			if (
				q &&
				!(
					m.title.toLowerCase().includes(q) ||
					m.author.toLowerCase().includes(q) ||
					m.genre.toLowerCase().includes(q) ||
					m.type.toLowerCase().includes(q)
				)
			)
				return false;
			if (types.length && !types.includes(m.type)) return false;
			if (genres.length && !genres.includes(m.genre)) return false;
			if (status !== 'any' && m.status !== status) return false;
			if (minRating && m.rating < minRating) return false;
			return true;
		});
		const sorters = {
			trending: (a: (typeof list)[0], b: (typeof list)[0]) => b.rating - a.rating || b.ch - a.ch,
			rating: (a: (typeof list)[0], b: (typeof list)[0]) => b.rating - a.rating,
			newest: (a: (typeof list)[0], b: (typeof list)[0]) => a.added - b.added,
			chapters: (a: (typeof list)[0], b: (typeof list)[0]) => b.ch - a.ch,
		};
		return [...list].sort(sorters[sort]);
	});

	const anyFilter = $derived(
		types.length > 0 || genres.length > 0 || status !== 'any' || minRating > 0,
	);
	const minRatingLabel = $derived(minRating > 0 ? minRating.toFixed(1) + '+' : 'Any');

	const activePills = $derived.by(() => {
		const pills: { label: string; remove: () => void }[] = [];
		types.forEach((t) =>
			pills.push({ label: `${FLAG[t]}  ${t}`, remove: () => (types = toggle(types, t)) }),
		);
		genres.forEach((g) => pills.push({ label: g, remove: () => (genres = toggle(genres, g)) }));
		if (status !== 'any')
			pills.push({ label: STATUS_META[status].label, remove: () => (status = 'any') });
		if (minRating)
			pills.push({ label: minRating.toFixed(1) + '+ rating', remove: () => (minRating = 0) });
		return pills;
	});

	const sortChips = [
		{ key: 'trending', label: 'Trending' },
		{ key: 'rating', label: 'Top rated' },
		{ key: 'newest', label: 'Newest' },
		{ key: 'chapters', label: 'Most ch.' },
	] as const;

	function resetFilters() {
		types = [];
		genres = [];
		status = 'any';
		minRating = 0;
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
		<div class="format-tabs">
			<button class="ftab" class:on={types.length === 0} onclick={() => (types = [])}
				>All formats</button
			>
			{#each TYPES as t (t)}
				<button
					class="ftab"
					class:on={types.length === 1 && types[0] === t}
					onclick={() => (types = [t])}
					title={t}
				>
					<span class="flag">{FLAG[t]}</span>
				</button>
			{/each}
		</div>
	</div>
</div>

<div class="body k-gutter">
	<aside class="rail">
		<div class="rail-head">
			<span class="rail-title">Filters</span>
			{#if anyFilter}<button class="reset" onclick={resetFilters}>Reset</button>{/if}
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
			<span class="glabel">Genre</span>
			<div class="chips">
				{#each ALL_GENRES as g (g)}
					<button
						class="chip"
						class:on={genres.includes(g)}
						onclick={() => (genres = toggle(genres, g))}>{g}</button
					>
				{/each}
			</div>
		</div>

		<div class="group">
			<span class="glabel">Status</span>
			<div class="chips">
				{#each ['any', 'ongoing', 'completed', 'hiatus'] as s (s)}
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
			<div class="rating-head">
				<span class="glabel">Minimum rating</span>
				<span class="rating-val">
					{#if minRating > 0}<Icon
							name="star"
							size={12}
							fill="var(--k-star)"
						/>{/if}{minRatingLabel}
				</span>
			</div>
			<input
				type="range"
				min="0"
				max="9.5"
				step="0.5"
				bind:value={minRating}
				class="rating-slider"
			/>
			<div class="rating-scale"><span>Any</span><span>9.5+</span></div>
		</div>
	</aside>

	<div class="results">
		<div class="results-head">
			<div class="results-title">
				<span class="rt">{query.trim() ? `"${query.trim()}"` : 'All series'}</span>
				<span class="rc">{loading ? 'Loading…' : `${results.length} series`}</span>
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

		{#if loading}
			<CardGridSkeleton count={12} />
		{:else if results.length}
			<div class="grid">
				{#each results as m (m.title)}
					<MangaCard
						title={m.title}
						sub={`${m.genre} · ${m.ch} ch`}
						rating={m.rating.toFixed(1)}
						cover={m.cover}
						id={m.id}
						flagType={m.type}
						status={{ label: STATUS_META[m.status].label, color: STATUS_META[m.status].color }}
					/>
				{/each}
			</div>
		{:else}
			<div class="empty">
				<div class="empty-icon"><Icon name="search" size={24} /></div>
				<div class="empty-title">No matches found</div>
				<div class="empty-desc">Try a different search term or loosen your filters.</div>
				<button class="empty-btn" onclick={resetAll}>Clear all</button>
			</div>
		{/if}
	</div>
</div>

<Footer links />

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
	.format-tabs {
		display: flex;
		gap: 10px;
		margin-top: 18px;
	}
	.ftab {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		height: 40px;
		padding: 0 18px;
		border-radius: 10px;
		font-weight: 700;
		font-size: 13.5px;
		cursor: pointer;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
		color: var(--k-text-3);
		transition: all 0.15s;
	}
	.ftab.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
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
	.rating-slider {
		width: 100%;
		accent-color: var(--k-star);
		cursor: pointer;
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
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(158px, 1fr));
		gap: 30px 22px;
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
	@media (max-width: 820px) {
		.body {
			grid-template-columns: 1fr;
			gap: 28px;
		}
		.rail {
			position: static;
		}
	}
</style>
