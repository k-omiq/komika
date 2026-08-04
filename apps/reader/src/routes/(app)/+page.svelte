<script lang="ts">
	import MangaCard from '$lib/components/MangaCard.svelte';
	import Cover from '$lib/components/Cover.svelte';
	import CardRowSkeleton from '$lib/components/CardRowSkeleton.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { slug, cardSub, cardTimeTooltip as timeTooltip, FLAG } from '$lib/data/types';
	import type { FeaturedView } from '$lib/data/source';

	let { data } = $props();

	/** Genre chips shown on a hero slide — the rest would wrap past the synopsis. */
	const HERO_GENRES = 4;

	// The home feeds are a resolved object on the server/hydration (edge SSR renders
	// real cards) and a pending Promise on client navigations (skeletons via {#await}).
	// See +page.ts. `HomeData` is the resolved shape either way.
	type HomeData = Awaited<typeof data.home>;

	let heroIndex = $state(0);
	// Once the reader picks a slide, stop auto-rotating so their choice sticks.
	let heroPicked = $state(false);
	// …and hold the rotation while they are actually AT the hero (pointer over it, or
	// keyboard focus inside it). Without this the banner swaps the slide out from under
	// whatever they are doing: mid-synopsis it is merely rude, but with focus on the
	// title link it is a correctness bug — the link the reader tabbed to keeps its DOM
	// node and its focus ring while its href is rewritten to a different series, so
	// pressing Enter opens something they never chose. (It is also the WCAG 2.2.2
	// "pause, stop, hide" mechanism for a 5s auto-advancing region.)
	let heroHeld = $state(false);
	/** Auto-rotation is stopped — either pinned by a click or held by the reader. */
	const heroPaused = $derived(heroPicked || heroHeld);

	/** Step the carousel by ±1, wrapping, and pin it to the reader's choice. */
	function stepHero(delta: number): void {
		const n = featured.length;
		if (!n) return;
		// Step from the CLAMPED index, never the raw one. The two differ exactly when the
		// feed shrank under a stationary `heroIndex` (see `slide`), and stepping from the
		// raw value then moves relative to a slide nobody is looking at: `heroIndex` 7 in a
		// 3-slide feed shows slide 2, and "next" would compute (7 + 1) % 3 = 2 — the same
		// slide. The click would do nothing visible while still setting `heroPicked`, i.e.
		// permanently stopping the rotation, so the hero would look frozen for good.
		heroIndex = (slide + delta + n) % n;
		heroPicked = true;
	}

	function seriesHref(f: FeaturedView): string {
		return `/series/${f.id ?? slug(f.title)}`;
	}

	// The hero slides. This is a $derived, NOT an $effect: effects don't run during
	// SSR, so populating `featured` from one left the edge-rendered HTML with no
	// cover, no title and no CTA — the single most prominent thing on the page was
	// blank in the server response (and in the s-maxage cache behind it), even
	// though the data was right there in `data.home`.
	//
	// `data.home` is a resolved object on the server and on hydration, and a pending
	// Promise on client-side navigations (see +page.ts); the promise arm still needs
	// an effect, but only that arm.
	const settled = $derived(data.home instanceof Promise ? undefined : (data.home as HomeData));
	let streamed = $state<HomeData | undefined>(undefined);
	$effect(() => {
		const h = data.home;
		streamed = undefined;
		if (!(h instanceof Promise)) return;
		let cancelled = false;
		h.then((r) => {
			if (!cancelled) streamed = r;
		});
		return () => {
			cancelled = true;
		};
	});
	// `data.home` never rejects (empty feeds on error), so an empty `featured` is the
	// backend-down / nothing-live case rather than a loading state — the hero is dropped
	// entirely there, it does not fall back to the skeleton.
	const featured = $derived<FeaturedView[]>((settled ?? streamed)?.featured ?? []);

	// Auto-rotate the hero, unless the reader stopped it or prefers reduced motion.
	// Re-runs (and so re-arms a fresh interval) whenever `featured` or the paused flag
	// changes; the teardown clears the old one, so there is exactly one timer at a time
	// and none at all after unmount.
	$effect(() => {
		if (heroPaused) return;
		if (featured.length < 2) return;
		const reduce =
			typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;
		if (reduce) return;
		const t = setInterval(() => {
			// From `slide`, not `heroIndex` — same reason as `stepHero`. Advancing the raw
			// index after the feed shrank would re-render the same slide and read as a
			// carousel that skipped a beat.
			heroIndex = (slide + 1) % featured.length;
		}, 5000);
		return () => clearInterval(t);
	});

	/**
	 * The active slide: `heroIndex` clamped into range, never indexed raw.
	 *
	 * `featured` is not a fixed length and this component does not own it — it is whatever
	 * the trending feed returned for the render in question, and it can change under a
	 * stationary `heroIndex`. A raw `featured[heroIndex]` past the end is `undefined`,
	 * which silently blanks the single most prominent element on the site (the `{#if
	 * current}` arm renders nothing at all) with no error anywhere to notice it by.
	 *
	 * Measured, so the next reader doesn't have to: today no in-app path re-lengths the
	 * feed under a LIVE instance. Navigating away and back destroys this component, so
	 * `heroIndex` returns to 0; clicking Home while already on `/` re-runs nothing at all,
	 * because `+page.ts`'s load reads neither `url` nor `params` and so has no dependency
	 * to invalidate. What makes the clamp worth its one `Math.min` anyway is that ALL of
	 * that is a property of the route's load, not of the hero: an `invalidateAll()` from a
	 * sign-in, a retry button like the one on /updates, or a `depends()` added later hands
	 * this component a shorter list while it is mounted, and the failure mode is silent.
	 *
	 * Clamping (rather than resetting to 0) is the right degradation for the reader who
	 * pinned a slide: they keep looking at the end of the feed they were reading, instead
	 * of being yanked back to the start. Its one hazard is that two `heroIndex` values then
	 * map to one slide, which is why both writers below step from `slide`, not `heroIndex`.
	 */
	const slide = $derived(Math.min(heroIndex, Math.max(0, featured.length - 1)));
	const current = $derived(featured[slide]);
</script>

<!-- Resolved object on the server (SSR renders cards) vs pending Promise on the
     client (skeletons via {#await}). Both feed the same `content` snippet. -->
{#if data.home instanceof Promise}
	{#await data.home}
		{@render loading()}
	{:then home}
		{@render content(home)}
	{/await}
{:else}
	{@render content(data.home)}
{/if}

{#snippet loading()}
	<!-- LOADING -->
	<section class="hero hero-loading">
		<div class="hero-body">
			<span class="eyebrow">trending</span>
			<div class="hero-grid">
				<div class="hero-cover k-skeleton"></div>
				<div class="hero-text">
					<div class="k-skeleton title-sk"></div>
					<div class="k-skeleton line-sk"></div>
					<div class="k-skeleton line-sk short"></div>
				</div>
			</div>
		</div>
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
{/snippet}

{#snippet content(home: HomeData)}
	<!-- HERO. Rendered only when there IS a slide: an empty `featured` (backend down, or
	     every discovery feed empty) would otherwise leave a 120px band containing the word
	     "trending" and nothing else — a header for content that does not exist. The rows
	     below have the same all-or-nothing treatment. -->
	{#if current}
		<section
			class="hero"
			aria-roledescription="carousel"
			aria-label="Featured series"
			onmouseenter={() => (heroHeld = true)}
			onmouseleave={() => (heroHeld = false)}
			onfocusin={() => (heroHeld = true)}
			onfocusout={() => (heroHeld = false)}
		>
			<!-- The slide's own cover, blown up and blurred, is the backdrop; the scrim over
			     it is `--k-bg`, so the wash (and therefore the text on top of it) follows the
			     active theme instead of forcing a dark plate into the light one. -->
			{#if current.cover}
				<div class="hero-bg" aria-hidden="true"><Cover src={current.cover} alt="" /></div>
			{/if}
			<div class="hero-scrim" aria-hidden="true"></div>
			<div class="hero-body">
				<span class="eyebrow">trending</span>
				<!-- `aria-live` is off while the carousel is advancing on its own (announcing a
				     slide every 5s would make the page unusable) and polite once it is paused —
				     which includes the moment an arrow is clicked, so a screen-reader user hears
				     the slide they just asked for. This is the APG carousel pattern. -->
				<div
					class="hero-grid"
					role="group"
					aria-roledescription="slide"
					aria-label={`Slide ${slide + 1} of ${featured.length}`}
					aria-live={heroPaused ? 'polite' : 'off'}
				>
					<a
						class="hero-cover"
						href={seriesHref(current)}
						aria-label={`Open ${current.title}`}
						tabindex="-1"
					>
						<Cover src={current.cover} alt={current.title} />
						<span class="hero-flag" title={current.type}>{FLAG[current.type]}</span>
					</a>
					<div class="hero-text">
						<h1><a href={seriesHref(current)}>{current.title}</a></h1>
						{#if current.genres.length}
							<div class="hero-tags">
								{#each current.genres.slice(0, HERO_GENRES) as g (g)}
									<a class="hero-tag" href={`/browse?genre=${encodeURIComponent(g)}`}>{g}</a>
								{/each}
							</div>
						{/if}
						{#if current.description}
							<p class="hero-desc">{current.description}</p>
						{/if}
						<div class="hero-foot">
							<!-- The credit line, in the same slot as the design's author name. Not every
							     source gives one, and italicising a chapter figure would read as an author,
							     so the fallback drops the italic. A chapter count of 0 is real and common
							     (MangaDex strips chapters on a licensing takedown), and "0 chapters" as a
							     credit is worse than no credit, so that case shows nothing.

							     "98 chapters", NOT "Ch. 98" — this was the last surviving F4 site.
							     `current.ch` is `Series.chapterCount` (see `toFeatured`), a COUNT of how
							     many chapters we hold; the old "Ch. 98" label announced it as the newest
							     chapter's NUMBER, which it is not. On a partially-mirrored series the two
							     are far apart. The fix is to LABEL it honestly rather than blank it: the
							     count is a true and useful fact, it was only wearing the wrong word. -->
							{#if current.author}
								<span class="hero-author">{current.author}</span>
							{:else if current.ch > 0}
								<span class="hero-author plain"
									>{current.ch} {current.ch === 1 ? 'chapter' : 'chapters'}</span
								>
							{/if}
							<div class="hero-nav">
								<span class="hero-no">NO. {slide + 1}</span>
								<!-- Disabled off `featured`, the same list `stepHero` steps through and
								     `slide` indexes. `home.featured` is the same array by every path that
								     can render this snippet, but it is a SECOND source of truth for a
								     control whose whole job is to move within the first one. -->
								<button
									class="hero-arrow"
									onclick={() => stepHero(-1)}
									disabled={featured.length < 2}
									aria-label="Previous featured series"
								>
									<Icon name="chevron-left" size={26} />
								</button>
								<button
									class="hero-arrow"
									onclick={() => stepHero(1)}
									disabled={featured.length < 2}
									aria-label="Next featured series"
								>
									<Icon name="chevron-right" size={26} />
								</button>
							</div>
						</div>
					</div>
				</div>
			</div>
		</section>
	{/if}

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
					{#each home.latestUpdates as item (item.id ?? item.title + item.ch)}
						<MangaCard
							title={item.title}
							sub={cardSub(item)}
							subTitle={timeTooltip(item)}
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
					{#each home.trending as item (item.id ?? item.title + item.ch)}
						<MangaCard
							title={item.title}
							sub={cardSub(item)}
							subTitle={timeTooltip(item)}
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
					{#each home.latestAdded as item (item.id ?? item.title + item.ch)}
						<MangaCard
							title={item.title}
							sub={cardSub(item, 'Added ')}
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
{/snippet}

<style>
	.hero {
		position: relative;
		overflow: hidden;
		background: var(--k-bg);
		/* Contain the backdrop's blur+scale inside the banner. */
		isolation: isolate;
	}
	/* The slide's cover, overscanned and blurred to a colour field. The overscan has to
	   exceed the blur's reach or the element's own transparent edge gets averaged in and
	   the backdrop fades out around the banner's rim: CSS `blur(r)` is a Gaussian with
	   standard deviation r, so it feathers ~3r = 144px inward. That is an ABSOLUTE
	   distance, which is why the inset is px and not the -10% it started as — 10% of a
	   300px-tall phone hero is 30px of overscan against 144px of feather, i.e. the top and
	   bottom thirds of the art washed away, worst exactly where the banner is shortest. */
	.hero-bg {
		position: absolute;
		inset: -150px;
		filter: blur(48px) saturate(1.4);
	}
	.hero-scrim {
		position: absolute;
		inset: 0;
		/* Theme token, not a fixed dark plate: in light mode this washes the cover art
		   out to a pale tint and the hero's text can stay on the normal text ramp.
		   0.88, not 0.84: the wash is what the hero's text contrast is computed against,
		   and the art behind it is arbitrary. A dark cover under the LIGHT theme is the
		   worst case — it drags the plate from #f4f2ee down toward #d3d1cd, and at 0.84
		   that put the dimmest text on the slide (the "Ch. 98" credit) at 4.07:1, under
		   the 4.5:1 floor. 12% art still reads as colour. */
		background: var(--k-bg);
		opacity: 0.88;
	}
	.head-sk {
		width: 180px;
		height: 22px;
		border-radius: 6px;
	}
	.hero-body {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 20px;
		padding: 40px var(--k-gutter) 44px;
	}
	.eyebrow {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 30px;
		letter-spacing: -0.02em;
		color: var(--k-text-bright);
	}
	.hero-grid {
		display: flex;
		align-items: stretch;
		gap: 34px;
		min-height: 360px;
	}
	.hero-cover {
		position: relative;
		flex: 0 0 auto;
		width: 240px;
		aspect-ratio: 2 / 3;
		border-radius: var(--k-radius-lg);
		overflow: hidden;
		background: var(--k-cover);
		box-shadow: 0 18px 40px rgba(0, 0, 0, 0.28);
	}
	.hero-flag {
		position: absolute;
		right: 8px;
		bottom: 8px;
		width: 26px;
		height: 26px;
		border-radius: var(--k-radius-sm);
		background: var(--k-glass-strong);
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 14px;
		line-height: 1;
	}
	.hero-text {
		flex: 1 1 auto;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.hero-text h1 {
		font-size: 46px;
		line-height: 1.04;
		letter-spacing: -0.03em;
		margin: 0;
	}
	.hero-text h1 a {
		color: var(--k-text-bright);
		text-decoration: none;
	}
	.hero-text h1 a:hover {
		color: var(--k-accent);
	}
	.hero-tags {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.hero-tag {
		padding: 5px 10px;
		border: 1px solid var(--k-border-3);
		border-radius: var(--k-radius-sm);
		background: var(--k-hover-fill);
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--k-text-muted);
		text-decoration: none;
	}
	.hero-tag:hover {
		border-color: var(--k-border-strong);
		color: var(--k-text);
	}
	.hero-desc {
		margin: 0;
		max-width: 78ch;
		font-size: 15.5px;
		line-height: 1.62;
		color: var(--k-text-2);
		/* Synopses run from one line to twenty; clamping keeps the banner's height
		   stable as the carousel rotates. */
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 5;
		line-clamp: 5;
		overflow: hidden;
	}
	.hero-foot {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 20px;
		/* Pin the credit + carousel controls to the bottom of the cover. */
		margin-top: auto;
		padding-top: 12px;
	}
	.hero-author {
		font-size: 19px;
		font-style: italic;
		color: var(--k-text-muted);
		/* A credit is one unbreakable token as far as flex is concerned (its min-content
		   width is its longest word), so a long name pushed the arrows off a 320px screen.
		   Let it shrink and ellipsize instead — the arrows are the interactive half. */
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.hero-author.plain {
		font-style: normal;
		font-size: 15px;
		/* Not --k-text-dim: see the scrim. This is the lowest-contrast text on the slide
		   and it sits over an arbitrary cover. */
		color: var(--k-text-muted);
	}
	.hero-nav {
		display: flex;
		align-items: center;
		gap: 10px;
		flex: 0 0 auto;
		/* Stay hard right even when the credit line is absent (no author and no chapter
		   count) — `space-between` alone would slide the controls left in that case. */
		margin-left: auto;
	}
	.hero-no {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 17px;
		letter-spacing: 0.06em;
		color: var(--k-text-bright);
		margin-right: 6px;
	}
	.hero-arrow {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 34px;
		height: 34px;
		padding: 0;
		border: none;
		border-radius: var(--k-radius-sm);
		background: none;
		color: var(--k-text-dim);
		cursor: pointer;
	}
	.hero-arrow:hover:not(:disabled) {
		color: var(--k-text-bright);
		background: var(--k-hover-fill);
	}
	.hero-arrow:disabled {
		color: var(--k-text-disabled);
		cursor: default;
	}
	/* The loading hero reuses `.hero-body` / `.hero-grid` / `.hero-cover` verbatim, so its
	   geometry is the real hero's by construction and the swap doesn't shift layout. */
	.title-sk {
		width: min(460px, 70%);
		height: 44px;
		border-radius: 8px;
	}
	.line-sk {
		width: min(620px, 90%);
		height: 14px;
		border-radius: 6px;
	}
	.line-sk.short {
		width: min(420px, 60%);
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
	@media (max-width: 900px) {
		.hero-grid {
			gap: 22px;
			min-height: 0;
		}
		.hero-cover {
			width: 150px;
		}
		.hero-text h1 {
			font-size: 32px;
		}
		.hero-desc {
			-webkit-line-clamp: 4;
			line-clamp: 4;
			font-size: 14.5px;
		}
	}
	@media (max-width: 640px) {
		.hero-body {
			padding-top: 28px;
			padding-bottom: 30px;
			gap: 16px;
		}
		.eyebrow {
			font-size: 24px;
		}
		.hero-grid {
			gap: 16px;
		}
		.hero-cover {
			width: 112px;
		}
		.hero-text {
			gap: 10px;
		}
		.hero-text h1 {
			font-size: 24px;
		}
		.hero-tag {
			font-size: 10px;
			padding: 4px 8px;
		}
		/* On a phone the cover column leaves no room for a synopsis beside it, so the
		   description moves below the whole grid and the credit row sits under it. */
		.hero-desc {
			font-size: 14px;
			-webkit-line-clamp: 3;
			line-clamp: 3;
		}
		.hero-foot {
			padding-top: 4px;
		}
		.hero-author {
			font-size: 15px;
		}
		.hero-no {
			font-size: 14px;
		}
		.hero-arrow {
			width: 30px;
			height: 30px;
		}
		.sections {
			gap: 40px;
			padding-top: 40px;
			padding-bottom: 56px;
		}
		.format-grid {
			grid-template-columns: 1fr;
			gap: 14px;
		}
		.format-card {
			padding: 20px 22px;
		}
		.block h2 {
			font-size: 19px;
		}
		/* Snappy, touch-friendly horizontal carousels. */
		.row {
			gap: 16px;
			scroll-snap-type: x proximity;
			-webkit-overflow-scrolling: touch;
		}
		.genres {
			gap: 18px 22px;
		}
	}
</style>
