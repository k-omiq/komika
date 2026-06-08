<script lang="ts">
	import Icon from '$lib/components/Icon.svelte';
	import Cover from '$lib/components/Cover.svelte';
	import StatusMenu from '$lib/components/StatusMenu.svelte';
	import { setLibraryStatus, setFavorite, type LibraryRowView } from '$lib/data/source';
	import { SHELF_META, slug, type Shelf } from '$lib/data/types';

	let { data } = $props();
	// `data.library` is resolved in `load` (this is a static SPA — streamed promises
	// don't resolve in the component, which is what left the page stuck loading), so
	// it's a ready value here. Copy the catalogue into local state so shelf/favourite
	// edits reflect immediately (optimistic), re-syncing whenever `load` re-runs.
	let rows = $state<LibraryRowView[]>([]);
	$effect(() => {
		rows = data.library.libraryCatalog.map((r) => ({ ...r }));
	});
	const continueRow = $derived(data.library.continueRow);

	let shelf = $state<'all' | Shelf | 'favorites'>('all');
	let sort = $state<'recent' | 'alpha'>('recent');

	const counts = $derived({
		all: rows.length,
		reading: rows.filter((c) => c.shelf === 'reading').length,
		completed: rows.filter((c) => c.shelf === 'completed').length,
		onhold: rows.filter((c) => c.shelf === 'onhold').length,
		plan: rows.filter((c) => c.shelf === 'plan').length,
		favorites: rows.filter((c) => c.favorite).length,
	});

	const items = $derived.by(() => {
		let list =
			shelf === 'all'
				? [...rows]
				: shelf === 'favorites'
					? rows.filter((c) => c.favorite)
					: rows.filter((c) => c.shelf === shelf);
		if (sort === 'alpha') list = [...list].sort((a, b) => a.title.localeCompare(b.title));
		return list.map((c) => {
			const m = SHELF_META[c.shelf];
			const progress = c.total ? Math.round((c.read / c.total) * 100) : 0;
			let sub: string;
			if (c.shelf === 'completed') sub = `Completed · ${c.total} ch`;
			else if (c.shelf === 'plan') sub = `${c.genre} · ${c.total} ch`;
			else sub = `Ch. ${c.read} / ${c.total}`;
			return {
				...c,
				meta: m,
				progress,
				sub,
				showBar: c.shelf === 'reading' || c.shelf === 'onhold',
			};
		});
	});

	const shelfDefs = [
		{ key: 'all', label: 'All' },
		{ key: 'reading', label: 'Reading' },
		{ key: 'completed', label: 'Completed' },
		{ key: 'onhold', label: 'On Hold' },
		{ key: 'plan', label: 'Plan to Read' },
		{ key: 'favorites', label: 'Favorites' },
	] as const;

	// Optimistically re-shelve / (un)favourite, persisting via the backend. On a
	// persisted failure the source layer resolves to the optimistic value, so the
	// UI stays consistent; a genuine desync corrects on the next load.
	async function changeShelf(id: string | undefined, next: Shelf): Promise<void> {
		if (!id) return;
		const row = rows.find((r) => r.id === id);
		if (!row || row.shelf === next) return;
		row.shelf = next;
		const result = await setLibraryStatus(id, next);
		if (result) row.shelf = result;
	}
	async function toggleFavorite(id: string | undefined, current: boolean): Promise<void> {
		if (!id) return;
		const row = rows.find((r) => r.id === id);
		if (!row) return;
		row.favorite = !current;
		row.favorite = await setFavorite(id, !current);
	}
</script>

<div class="head k-gutter">
	<div>
		<h1>Your Library</h1>
		<div class="subtitle">{counts.all} series · {counts.reading} in progress</div>
	</div>
</div>

{#if continueRow.length}
	<section class="continue k-gutter">
		<h2 class="section-label">Continue Reading</h2>
		<div class="continue-row">
			{#each continueRow as item (item.id ?? item.title)}
				<a class="continue-card" href={`/series/${item.id ?? slug(item.title)}`}>
					<div class="landscape k-cover">
						<Cover src={item.cover} alt={item.title} loading="lazy" />
						<span class="resume"><Icon name="play" size={9} fill="currentColor" />Resume</span>
						<div class="bar"><div class="fill" style="width:{item.progress}%"></div></div>
					</div>
					<div class="continue-meta">
						<span class="ctitle">{item.title}</span>
						<span class="cch">{item.ch}</span>
					</div>
					<div class="csub">{item.progress}% · {item.genre}</div>
				</a>
			{/each}
		</div>
	</section>
{/if}

<div class="shelfbar k-gutter">
	<div class="shelf-tabs">
		{#each shelfDefs as d (d.key)}
			<button
				class="shelf-tab"
				class:on={shelf === d.key}
				onclick={() => (shelf = d.key as 'all' | Shelf)}
			>
				{d.label}<span class="count">{counts[d.key]}</span>
			</button>
		{/each}
	</div>
	<div class="sort">
		<span class="sort-label">Sort</span>
		<button class="sortchip" class:on={sort === 'recent'} onclick={() => (sort = 'recent')}
			>Recent</button
		>
		<button class="sortchip" class:on={sort === 'alpha'} onclick={() => (sort = 'alpha')}
			>A–Z</button
		>
	</div>
</div>

<div class="grid-wrap k-gutter">
	{#if items.length}
		<div class="grid">
			{#each items as item (item.id ?? item.title + item.shelf)}
				{@const href = `/series/${item.id ?? slug(item.title)}`}
				<div class="lib-card">
					<div class="cover k-cover">
						<Cover src={item.cover} alt={item.title} loading="lazy" />
						<a class="cover-link" {href} aria-label={item.title}></a>
						<span
							class="badge"
							style="color:{item.meta.color};background:{item.meta.bg};border-color:{item.meta
								.border}">{item.meta.label}</span
						>
						<span class="rating"
							><Icon name="star" size={10} fill="var(--k-star)" />{item.rating}</span
						>
						<button
							class="fav-chip"
							class:on={item.favorite}
							aria-pressed={item.favorite}
							aria-label={item.favorite ? 'Remove from favourites' : 'Add to favourites'}
							title={item.favorite ? 'Favourited' : 'Add to favourites'}
							onclick={() => toggleFavorite(item.id, item.favorite)}
						>
							<Icon name="heart" size={14} fill={item.favorite ? 'currentColor' : 'none'} />
						</button>
						<div class="status-slot">
							<StatusMenu status={item.shelf} onchange={(s) => changeShelf(item.id, s)} />
						</div>
						{#if item.showBar}
							<div class="progressbar"><div class="fill" style="width:{item.progress}%"></div></div>
						{/if}
					</div>
					<a class="lib-title" {href}>{item.title}</a>
					<div class="lib-sub">{item.sub}</div>
				</div>
			{/each}
		</div>
	{:else}
		<div class="empty">
			<div class="empty-icon"><Icon name="bookmark" size={24} /></div>
			<div class="empty-title">
				{counts.all === 0 ? 'Your library is empty' : 'Nothing on this shelf yet'}
			</div>
			<div class="empty-desc">
				{counts.all === 0
					? 'Add series to your library and they’ll show up here, sorted by shelf.'
					: 'Move a series onto this shelf, or browse for something new to read.'}
			</div>
			<a class="empty-btn" href="/browse">Browse series</a>
		</div>
	{/if}
</div>


<style>
	.head {
		padding-top: 52px;
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 24px;
		flex-wrap: wrap;
	}
	h1 {
		font-size: 40px;
		margin: 0 0 6px;
		color: var(--k-text-bright);
	}
	.subtitle {
		font-size: 14px;
		color: var(--k-text-dimmer);
	}
	.section-label {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 16px;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		margin: 0;
		color: var(--k-text-dimmer);
	}
	.continue {
		display: flex;
		flex-direction: column;
		gap: 18px;
		padding-top: 40px;
	}
	.continue-row {
		display: flex;
		gap: 20px;
		overflow-x: auto;
		padding-bottom: 4px;
	}
	.continue-card {
		flex: 0 0 auto;
		width: 300px;
		text-decoration: none;
	}
	.landscape {
		position: relative;
		width: 300px;
		height: 168px;
		border-radius: 10px;
		transition: border-color 0.2s;
	}
	/* Round the cover art to match its container without `overflow:hidden`, which
	   would clip the StatusMenu dropdown that escapes below the grid cover box. */
	.landscape :global(.cover-img),
	.landscape :global(.cover-ph) {
		border-radius: 10px;
	}
	.cover :global(.cover-img),
	.cover :global(.cover-ph) {
		border-radius: 8px;
	}
	.continue-card:hover .landscape {
		border-color: rgba(255, 255, 255, 0.2);
	}
	.resume {
		position: absolute;
		left: 12px;
		top: 11px;
		display: inline-flex;
		align-items: center;
		gap: 6px;
		background: rgba(12, 12, 13, 0.68);
		padding: 4px 9px;
		border-radius: 6px;
		font-size: 11px;
		font-weight: 700;
		color: var(--k-text);
	}
	.bar {
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		height: 4px;
		background: rgba(255, 255, 255, 0.12);
	}
	.bar .fill,
	.progressbar .fill {
		height: 100%;
		background: var(--k-primary);
	}
	.continue-meta {
		margin-top: 11px;
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 10px;
	}
	.ctitle {
		font-weight: 600;
		font-size: 14.5px;
		color: var(--k-text-1);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.cch {
		flex: 0 0 auto;
		font-size: 12px;
		color: var(--k-text-faint);
	}
	.csub {
		font-size: 12px;
		color: var(--k-text-fainter);
		margin-top: 3px;
	}
	.shelfbar {
		padding-top: 36px;
		margin-top: 48px;
		border-top: 1px solid var(--k-border);
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
		flex-wrap: wrap;
	}
	.shelf-tabs {
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
	}
	.shelf-tab {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		font-size: 13.5px;
		font-weight: 700;
		padding: 8px 16px;
		border-radius: var(--k-radius-pill);
		cursor: pointer;
		background: transparent;
		color: var(--k-text-dimmer);
		border: 1px solid var(--k-border-4);
		transition: all 0.15s;
	}
	.shelf-tab.on {
		background: var(--k-primary);
		color: var(--k-on-primary);
		border-color: var(--k-primary);
	}
	.count {
		opacity: 0.55;
		font-weight: 600;
	}
	.sort {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.sort-label {
		font-size: 13px;
		color: var(--k-text-faint);
		margin-right: 6px;
	}
	.sortchip {
		font-size: 13px;
		font-weight: 700;
		padding: 7px 14px;
		border-radius: var(--k-radius-pill);
		cursor: pointer;
		background: transparent;
		color: var(--k-text-dimmer);
		border: 1px solid var(--k-border-2);
		transition: all 0.15s;
	}
	.sortchip.on {
		background: rgba(255, 255, 255, 0.1);
		color: var(--k-text);
		border-color: rgba(255, 255, 255, 0.2);
	}
	.grid-wrap {
		padding-top: 28px;
		padding-bottom: 80px;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
		gap: 26px 24px;
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
		max-width: 340px;
		line-height: 1.5;
	}
	.empty-btn {
		margin-top: 6px;
		height: 42px;
		padding: 0 22px;
		display: inline-flex;
		align-items: center;
		border-radius: 8px;
		background: var(--k-primary);
		border: none;
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 13.5px;
		text-decoration: none;
		cursor: pointer;
	}
	.lib-card {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.cover {
		position: relative;
		width: 100%;
		aspect-ratio: 2 / 3;
		border-radius: 8px;
		transition: opacity 0.15s;
	}
	/* Stretched navigation target — fills the cover, sits under the controls. */
	.cover-link {
		position: absolute;
		inset: 0;
		z-index: 1;
		border-radius: 8px;
	}
	.lib-card:hover .cover-link {
		background: rgba(12, 12, 13, 0.12);
	}
	.fav-chip {
		position: absolute;
		z-index: 2;
		left: 9px;
		bottom: 9px;
		width: 28px;
		height: 28px;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: 7px;
		cursor: pointer;
		color: var(--k-text);
		background: rgba(12, 12, 13, 0.72);
		border: 1px solid var(--k-border-3);
		backdrop-filter: blur(3px);
		transition: all 0.12s;
	}
	.fav-chip:hover {
		color: #e96e6e;
		border-color: rgba(233, 110, 110, 0.5);
	}
	.fav-chip.on {
		color: #e96e6e;
		background: rgba(233, 110, 110, 0.2);
		border-color: rgba(233, 110, 110, 0.55);
	}
	.status-slot {
		position: absolute;
		z-index: 2;
		right: 9px;
		bottom: 9px;
	}
	.lib-title {
		text-decoration: none;
	}
	.badge {
		position: absolute;
		z-index: 2;
		top: 9px;
		left: 9px;
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.04em;
		border: 1px solid;
		border-radius: 5px;
		padding: 3px 8px;
	}
	.rating {
		position: absolute;
		z-index: 2;
		top: 9px;
		right: 9px;
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-weight: 700;
		font-size: 11.5px;
		color: var(--k-text);
		background: rgba(12, 12, 13, 0.72);
		border-radius: 5px;
		padding: 3px 7px;
	}
	.progressbar {
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		height: 4px;
		background: rgba(12, 12, 13, 0.55);
	}
	.lib-title {
		font-weight: 600;
		font-size: 13.5px;
		color: var(--k-text-1);
		line-height: 1.25;
	}
	.lib-sub {
		font-size: 12px;
		color: var(--k-text-faint);
		margin-top: 3px;
	}
</style>
