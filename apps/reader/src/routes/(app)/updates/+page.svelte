<script lang="ts">
	import Icon from '$lib/components/Icon.svelte';
	import MangaCard from '$lib/components/MangaCard.svelte';
	import CardRowSkeleton from '$lib/components/CardRowSkeleton.svelte';
	import CardGridSkeleton from '$lib/components/CardGridSkeleton.svelte';
	import { FLAG, type ComicType } from '$lib/data/types';

	type UpdatesData = Awaited<ReturnType<typeof import('$lib/data/source').getUpdates>>;

	let { data } = $props();
	// Stream the feeds in; show skeletons until they resolve. Never rejects.
	let trendingGroups = $state<UpdatesData['trendingGroups']>([]);
	let newUpdates = $state<UpdatesData['newUpdates']>([]);
	let hotUpdates = $state<UpdatesData['hotUpdates']>([]);
	let loading = $state(true);
	$effect(() => {
		loading = true;
		data.updates.then((u) => {
			trendingGroups = u.trendingGroups;
			newUpdates = u.newUpdates;
			hotUpdates = u.hotUpdates;
			loading = false;
		});
	});

	let tab = $state<'new' | 'hot'>('new');
	let typeFilter = $state<'__all' | ComicType>('__all');

	const TYPES: ComicType[] = ['Manga', 'Manhwa', 'Manhua'];

	const updates = $derived.by(() => {
		let list = tab === 'hot' ? hotUpdates : newUpdates;
		// Cards carry their format from the live feed; ones without it (e.g. the
		// canonical-updates mirror) only show under "All".
		if (typeFilter !== '__all') list = list.filter((u) => u.type === typeFilter);
		return list;
	});
</script>

<div class="trending k-gutter">
	{#if loading}
		<section class="tgroup">
			<div class="k-skeleton head-sk"></div>
			<CardRowSkeleton />
		</section>
	{:else}
		{#each trendingGroups as tg (tg.label)}
			<section class="tgroup">
				<h2 class="section-label">{tg.label}</h2>
				<div class="marquee">
					{#each tg.items as item (item.title)}
						<MangaCard
							title={item.title}
							sub={`${item.ch} · ${item.time}`}
							rating={item.rating}
							cover={item.cover}
							id={item.id}
							fixed
						/>
					{/each}
				</div>
			</section>
		{/each}
	{/if}
</div>

<div class="updates k-gutter">
	<div class="updates-head">
		<h2 class="section-label">Updates</h2>
		<div class="tabs">
			<button class="tab" class:on={tab === 'new'} onclick={() => (tab = 'new')}>New</button>
			<button class="tab" class:on={tab === 'hot'} onclick={() => (tab = 'hot')}>
				<Icon name="flame" size={13} />Hot
			</button>
		</div>
	</div>

	<div class="type-tabs">
		<button class="ttab" class:on={typeFilter === '__all'} onclick={() => (typeFilter = '__all')}
			>All</button
		>
		{#each TYPES as t (t)}
			<button class="ttab" class:on={typeFilter === t} onclick={() => (typeFilter = t)} title={t}>
				<span class="flag">{FLAG[t]}</span>
			</button>
		{/each}
	</div>

	{#if loading}
		<CardGridSkeleton count={12} min={150} />
	{:else if updates.length}
		<div class="grid">
			{#each updates as item (item.title + item.ch)}
				<MangaCard
					title={item.title}
					sub={`${item.ch} · ${item.time}`}
					rating={item.rating}
					cover={item.cover}
					id={item.id}
					flagEmoji={item.type ? FLAG[item.type] : ''}
				/>
			{/each}
		</div>
	{:else}
		<div class="empty">
			<div class="empty-icon"><Icon name="flame" size={22} /></div>
			<div class="empty-title">No updates here</div>
			<div class="empty-desc">
				Nothing matches this filter right now — try another format or tab.
			</div>
			<button
				class="empty-btn"
				onclick={() => {
					typeFilter = '__all';
					tab = 'new';
				}}>Reset filters</button
			>
		</div>
	{/if}
</div>


<style>
	.trending {
		display: flex;
		flex-direction: column;
		gap: 48px;
		padding-top: 56px;
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
	.tgroup {
		display: flex;
		flex-direction: column;
		gap: 20px;
	}
	.marquee {
		display: flex;
		gap: 22px;
		overflow-x: auto;
		padding-bottom: 4px;
	}
	.updates {
		display: flex;
		flex-direction: column;
		gap: 20px;
		padding-top: 48px;
		padding-bottom: 80px;
		margin-top: 48px;
		border-top: 1px solid var(--k-border);
	}
	.updates-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
	}
	.tabs {
		display: flex;
		gap: 8px;
	}
	.tab {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-size: 13px;
		font-weight: 700;
		padding: 7px 15px;
		border-radius: var(--k-radius-pill);
		cursor: pointer;
		background: transparent;
		color: var(--k-text-dimmer);
		border: 1px solid var(--k-border-4);
		transition: all 0.15s;
	}
	.tab.on {
		background: var(--k-primary);
		color: var(--k-on-primary);
		border-color: var(--k-primary);
	}
	.type-tabs {
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
	}
	.ttab {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		font-size: 12.5px;
		font-weight: 700;
		padding: 7px 14px;
		border-radius: var(--k-radius-pill);
		cursor: pointer;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-dimmer);
		transition: all 0.15s;
	}
	.ttab.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.flag {
		font-size: 17px;
		line-height: 1;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
		gap: 24px;
	}
	.head-sk {
		width: 180px;
		height: 20px;
		border-radius: 6px;
	}
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 14px;
		padding: 72px 20px;
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
</style>
