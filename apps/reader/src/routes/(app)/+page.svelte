<script lang="ts">
	import MangaCard from '$lib/components/MangaCard.svelte';
	import Footer from '$lib/components/Footer.svelte';
	import Cover from '$lib/components/Cover.svelte';
	import CardRowSkeleton from '$lib/components/CardRowSkeleton.svelte';
	import { slug } from '$lib/data/types';
	import type { FeaturedView } from '$lib/data/source';

	let { data } = $props();

	let heroIndex = $state(0);
	// Featured slides, populated once the home feeds resolve. Kept as its own
	// state so the auto-rotate effect can depend on its length.
	let featured = $state<FeaturedView[]>([]);

	function seriesHref(f: FeaturedView): string {
		return `/series/${f.id ?? slug(f.title)}`;
	}

	// Mirror the resolved featured list into local state (drives the hero +
	// auto-rotate). `data.home` never rejects (empty results on error); an empty
	// featured list keeps the placeholder hero (same look as the loading hero).
	$effect(() => {
		data.home.then((h) => {
			featured = h.featured;
		});
	});

	// Auto-rotate the hero, unless the user prefers reduced motion.
	$effect(() => {
		if (featured.length < 2) return;
		const reduce =
			typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;
		if (reduce) return;
		const t = setInterval(() => {
			heroIndex = (heroIndex + 1) % featured.length;
		}, 5000);
		return () => clearInterval(t);
	});

	const current = $derived(featured[heroIndex]);
</script>

{#await data.home}
	<!-- LOADING -->
	<section class="hero hero-loading">
		<span class="cover-tag">COVER</span>
		<div class="hero-fade"></div>
	</section>
	<div class="sections k-gutter">
		<section class="block">
			<div class="k-skeleton head-sk"></div>
			<CardRowSkeleton />
		</section>
		<section class="block">
			<div class="k-skeleton head-sk"></div>
			<CardRowSkeleton />
		</section>
		<section class="block">
			<div class="k-skeleton head-sk"></div>
			<CardRowSkeleton />
		</section>
	</div>
{:then home}
	<!-- HERO -->
	<section class="hero">
		{#if current?.cover}
			<div class="hero-cover"><Cover src={current.cover} alt={current.title} /></div>
		{:else}
			<span class="cover-tag">COVER</span>
		{/if}
		<div class="hero-fade"></div>
		{#if current}
			<div class="hero-info">
				<h1>{current.title}</h1>
				<div class="hero-sub">{current.genre} — Ch. {current.ch}</div>
				<div class="hero-cta">
					<a class="btn-read" href={seriesHref(current)}>Read</a>
					<a class="btn-plus" href={seriesHref(current)} aria-label="Add to library">+</a>
				</div>
			</div>
			<div class="dots">
				{#each home.featured as _f, i (i)}
					<button class="dot" aria-label={`Slide ${i + 1}`} onclick={() => (heroIndex = i)}>
						<span class="bar" class:active={i === heroIndex}></span>
					</button>
				{/each}
			</div>
		{/if}
	</section>

	<div class="sections k-gutter">
		<!-- BROWSE BY FORMAT -->
		<section class="block">
			<h2>Browse by format</h2>
			<div class="format-grid">
				{#each home.formatCards as f (f.type)}
					<a class="format-card" href={`/browse?type=${f.type}`} style="--hover:{f.hover}">
						<span class="glow" style="background:radial-gradient(circle, {f.glow}, transparent 65%)"
						></span>
						<span class="format-name"><span class="format-flag">{f.flag}</span>{f.name}</span>
						<span class="format-desc">{f.desc}</span>
						<span class="format-count">{f.count}</span>
					</a>
				{/each}
			</div>
		</section>

		<!-- LATEST UPDATES -->
		{#if home.latestUpdates.length}
			<section class="block">
				<div class="row-head">
					<h2>Latest Updates</h2>
					<a class="view-all" href="/updates">View all</a>
				</div>
				<div class="row">
					{#each home.latestUpdates as item (item.title + item.ch)}
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
		{/if}

		<!-- TRENDING -->
		{#if home.trending.length}
			<section class="block">
				<h2>Trending</h2>
				<div class="row">
					{#each home.trending as item (item.title + item.ch)}
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
		{/if}

		<!-- LATEST ADDED -->
		{#if home.latestAdded.length}
			<section class="block">
				<h2>Latest Added</h2>
				<div class="row">
					{#each home.latestAdded as item (item.title + item.ch)}
						<MangaCard
							title={item.title}
							sub={`${item.ch} · Added ${item.time}`}
							rating={item.rating}
							cover={item.cover}
							id={item.id}
							fixed
						/>
					{/each}
				</div>
			</section>
		{/if}

		<!-- GENRES -->
		{#if home.homeGenres.length}
			<section class="block">
				<h2>Genres</h2>
				<div class="genres">
					{#each home.homeGenres as g (g)}
						<a class="genre" href={`/browse?genre=${encodeURIComponent(g)}`}>{g}</a>
					{/each}
				</div>
			</section>
		{/if}
	</div>
{/await}

<Footer />

<style>
	.hero {
		position: relative;
		height: 640px;
		overflow: hidden;
		background: #111112;
		background-image: repeating-linear-gradient(
			135deg,
			rgba(255, 255, 255, 0.03) 0 2px,
			transparent 2px 13px
		);
	}
	.hero-loading {
		height: 640px;
	}
	.hero-cover {
		position: absolute;
		inset: 0;
	}
	.head-sk {
		width: 180px;
		height: 22px;
		border-radius: 6px;
	}
	.cover-tag {
		position: absolute;
		left: var(--k-gutter);
		top: 28px;
		font-family: var(--k-font-mono);
		font-size: 10.5px;
		letter-spacing: 0.16em;
		color: rgba(255, 255, 255, 0.22);
	}
	.hero-fade {
		position: absolute;
		inset: 0;
		background: linear-gradient(to top, #0c0c0d 8%, rgba(12, 12, 13, 0.55) 42%, transparent 70%);
	}
	.hero-info {
		position: absolute;
		left: var(--k-gutter);
		bottom: 64px;
		max-width: 640px;
	}
	.hero-info h1 {
		font-size: 68px;
		line-height: 1.02;
		letter-spacing: -0.03em;
		margin: 0 0 14px;
		/* The hero is an always-dark cover-art scrim in both themes, so its text
		   stays light regardless of the active theme. */
		color: #f7f6f3;
	}
	.hero-sub {
		font-size: 15px;
		color: rgba(247, 246, 243, 0.72);
		margin-bottom: 28px;
	}
	.hero-cta {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	/* Hero CTAs sit on the always-dark cover scrim, so they use fixed light
	   values (not theme tokens) and look identical in both themes. */
	.btn-read {
		height: 48px;
		padding: 0 28px;
		border: none;
		border-radius: 8px;
		background: #f2f1ee;
		color: #0c0c0d;
		font-weight: 700;
		font-size: 15px;
		display: inline-flex;
		align-items: center;
		text-decoration: none;
	}
	.btn-read:hover {
		background: #ffffff;
	}
	.btn-plus {
		width: 48px;
		height: 48px;
		border-radius: 8px;
		background: transparent;
		border: 1px solid rgba(255, 255, 255, 0.18);
		color: #f2f1ee;
		font-size: 18px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		text-decoration: none;
	}
	.btn-plus:hover {
		border-color: rgba(255, 255, 255, 0.4);
	}
	.dots {
		position: absolute;
		right: var(--k-gutter);
		bottom: 70px;
		display: flex;
		gap: 6px;
	}
	.dot {
		background: none;
		border: none;
		cursor: pointer;
		padding: 8px 3px;
	}
	.dot .bar {
		display: block;
		width: 13px;
		height: 2px;
		background: rgba(255, 255, 255, 0.22);
		border-radius: 2px;
		transition: width 0.2s;
	}
	.dot .bar.active {
		width: 26px;
		background: #f2f1ee;
	}
	.sections {
		display: flex;
		flex-direction: column;
		gap: 60px;
		padding-top: 64px;
		padding-bottom: 80px;
	}
	.block {
		display: flex;
		flex-direction: column;
		gap: 20px;
	}
	.block h2 {
		font-size: 21px;
		margin: 0;
		color: var(--k-text);
	}
	.row-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
	}
	.view-all {
		font-size: 13px;
		color: var(--k-text-dimmer);
		text-decoration: none;
	}
	.view-all:hover {
		color: var(--k-text);
	}
	.format-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 20px;
	}
	.format-card {
		position: relative;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		gap: 8px;
		border: 1px solid var(--k-border-1);
		border-radius: 14px;
		padding: 24px 26px;
		background: var(--k-surface);
		text-decoration: none;
		transition: all 0.18s;
	}
	.format-card:hover {
		border-color: var(--hover);
		transform: translateY(-3px);
	}
	.glow {
		position: absolute;
		top: -24px;
		right: -24px;
		width: 110px;
		height: 110px;
		border-radius: 50%;
	}
	.format-name {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 20px;
		color: var(--k-text-bright);
	}
	.format-flag {
		font-size: 23px;
		line-height: 1;
	}
	.format-desc {
		font-size: 13px;
		color: var(--k-text-dimmer);
		line-height: 1.5;
	}
	.format-count {
		font-size: 12.5px;
		color: var(--k-text-fainter);
		margin-top: 2px;
	}
	.row {
		display: flex;
		gap: 22px;
		overflow-x: auto;
		padding-bottom: 4px;
	}
	.genres {
		display: flex;
		gap: 28px;
		flex-wrap: wrap;
	}
	.genre {
		font-size: 15px;
		color: var(--k-text-muted);
		text-decoration: none;
		padding-bottom: 2px;
		border-bottom: 1px solid transparent;
	}
	.genre:hover {
		color: var(--k-text);
		border-bottom-color: rgba(255, 255, 255, 0.3);
	}
	@media (max-width: 640px) {
		.hero {
			height: 460px;
		}
		.hero-info h1 {
			font-size: 40px;
		}
	}
</style>
