<script lang="ts">
	import { page } from '$app/state';
	import { untrack } from 'svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Stars from '$lib/components/Stars.svelte';
	import MangaCard from '$lib/components/MangaCard.svelte';
	import Footer from '$lib/components/Footer.svelte';
	import Cover from '$lib/components/Cover.svelte';
	import CommentThread from '$lib/components/CommentThread.svelte';
	import { FLAG } from '$lib/data/mock';
	import { setLibraryMark } from '$lib/data/source';
	import { auth } from '$lib/auth.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import {
		socialLive,
		loadSeriesSocial,
		saveSeriesRating,
		submitSeriesReview,
		type ReviewView,
	} from '$lib/data/social-repo';

	import type { SeriesView } from '$lib/data/source';

	let { data } = $props();
	// Stream the series detail; the page shows a hero/chapter skeleton until it
	// resolves. `data.series` never rejects (mock fallback on error).
	let view = $state<SeriesView | null>(null);
	let loading = $state(true);
	$effect(() => {
		loading = true;
		view = null;
		data.series.then((v) => {
			view = v;
			loading = false;
		});
	});

	const detail = $derived(view?.detail);
	const seriesDetail = $derived(view?.detail); // alias: template reads seriesDetail.author/artist/votes/…
	const title = $derived(detail?.title ?? '');
	const type = $derived(detail?.type ?? 'Manga');
	const rating = $derived(detail?.rating ?? '');
	const totalCh = $derived(detail?.totalCh ?? 0);
	const statusLabel = $derived(detail?.statusLabel ?? '');
	const continueCh = $derived(detail?.continueCh ?? 1);
	const relatedSeries = $derived(view?.related ?? []);

	function chapterHref(chId?: string) {
		return `/read/${detail?.id ?? data.slug}${chId ? `?ch=${chId}` : ''}`;
	}
	const readHref = $derived(chapterHref(detail?.startChapterId));

	let inLibrary = $state(false);
	let marking = $state(false);
	let userRating = $state(0);
	let myBody = $state('');
	let sort = $state<'newest' | 'oldest'>('newest');
	let reviews = $state<ReviewView[]>([]);
	let draft = $state('');
	let spoiler = $state(false);
	let posting = $state(false);
	let postError = $state<string | null>(null);
	let revealed = $state<Record<string, boolean>>({});

	const seriesId = $derived(detail?.id ?? '');
	const socialKey = $derived(data.slug ?? page.params.slug ?? '');
	const needsAuth = $derived(socialLive() && !auth.user);

	// Reflect the backend's library state on load (resets when navigating series).
	$effect(() => {
		const marked = detail?.isMarked ?? false;
		untrack(() => (inLibrary = marked));
	});

	async function toggleLibrary(): Promise<void> {
		if (marking) return;
		const next = !inLibrary;
		inLibrary = next; // optimistic
		marking = true;
		try {
			inLibrary = await setLibraryMark(seriesId, next);
		} finally {
			marking = false;
		}
	}

	// Load ratings + reviews for this series (re-runs on navigation and sign-in).
	$effect(() => {
		const id = seriesId;
		const key = socialKey;
		void auth.user?.id; // reload after auth changes so "mine" resolves
		if (!id) return; // series still streaming in
		let cancelled = false;
		loadSeriesSocial(id, key)
			.then((s) => {
				if (cancelled) return;
				userRating = s.myScore;
				myBody = s.myBody;
				reviews = s.reviews;
				revealed = {};
			})
			.catch(() => {});
		return () => {
			cancelled = true;
		};
	});

	function onRate(v: number): void {
		if (needsAuth) return;
		saveSeriesRating(seriesId, socialKey, v, myBody).catch(() => {});
	}
	function clearRating(): void {
		userRating = 0;
	}

	const chapters = $derived.by(() => {
		const chs = view?.chapters ?? [];
		return sort === 'oldest' ? [...chs].reverse() : chs;
	});

	const ratingLabel = $derived(
		needsAuth
			? 'Sign in to rate this series'
			: userRating > 0
				? `You rated ${userRating} / 10 · tap a star to update`
				: 'Tap a star to rate — out of 10',
	);

	const postDisabled = $derived(
		posting || draft.trim().length === 0 || (socialLive() && userRating === 0),
	);

	async function postReview(): Promise<void> {
		const body = draft.trim();
		if (!body || postDisabled) return;
		posting = true;
		postError = null;
		try {
			reviews = await submitSeriesReview(seriesId, socialKey, userRating, body, spoiler, reviews);
			myBody = body;
			draft = '';
			spoiler = false;
		} catch (err) {
			postError = err instanceof Error ? err.message : 'Could not post your review.';
		} finally {
			posting = false;
		}
	}
	function toggleLike(id: string): void {
		reviews = reviews.map((c) =>
			c.id === id ? { ...c, liked: !c.liked, likes: c.likes + (c.liked ? -1 : 1) } : c,
		);
	}
	function reveal(id: string): void {
		revealed = { ...revealed, [id]: true };
	}
</script>

{#if loading}
	<!-- LOADING -->
	<section class="backdrop">
		<span class="kv-tag">KEY VISUAL · 2400×1350</span>
		<div class="fade-top"></div>
		<div class="fade-left"></div>
	</section>
	<div class="hero k-gutter">
		<div class="hero-row">
			<div class="poster k-skeleton"></div>
			<div class="info skel-info">
				<div class="k-skeleton sk-badges"></div>
				<div class="k-skeleton sk-h1"></div>
				<div class="k-skeleton sk-creators"></div>
				<div class="k-skeleton sk-facts"></div>
				<div class="k-skeleton sk-cta"></div>
			</div>
		</div>
	</div>
{:else if detail && seriesDetail}
	<!-- backdrop -->
	<section class="backdrop">
		<span class="kv-tag">KEY VISUAL · 2400×1350</span>
		<div class="fade-top"></div>
		<div class="fade-left"></div>
	</section>

	<!-- hero info -->
	<div class="hero k-gutter">
		<div class="hero-row">
			<div class="poster"><Cover src={detail.cover} alt={title} /></div>
			<div class="info">
				<div class="badges">
					<span class="badge status"><span class="status-dot"></span>{statusLabel}</span>
					<span class="badge type"><span class="type-flag">{FLAG[type]}</span>{type}</span>
				</div>
				<h1>{title}</h1>
				<div class="creators">{seriesDetail.author} · {seriesDetail.artist}</div>
				<div class="facts">
					<span class="fact rating"
						><Icon name="star" size={16} fill="var(--k-star)" />{rating}<span class="votes"
							>({seriesDetail.votes})</span
						></span
					>
					<span class="sep"></span>
					<span class="fact">{totalCh} chapters</span>
					<span class="sep"></span>
					<span class="fact">Updated {seriesDetail.updated}</span>
				</div>
				<div class="cta">
					<a class="read" href={readHref}
						><Icon name="play" size={14} fill="currentColor" />Continue Ch. {continueCh}</a
					>
					<button class="lib" class:in={inLibrary} disabled={marking} onclick={toggleLibrary}>
						{#if inLibrary}
							<Icon name="check" size={16} strokeWidth={2.4} />In Library
						{:else}
							<Icon name="plus" size={16} strokeWidth={2.2} />Add to Library
						{/if}
					</button>
					<button class="share" aria-label="Share"><Icon name="share" size={18} /></button>
				</div>
			</div>
		</div>
	</div>

	<!-- genres + synopsis -->
	<div class="genres-syn k-gutter">
		<div class="genre-chips">
			{#each seriesDetail.genres as g (g)}
				<span class="gchip">{g}</span>
			{/each}
		</div>
		<p class="synopsis">{seriesDetail.synopsis}</p>
	</div>

	<!-- rate -->
	<div class="rate-wrap k-gutter">
		<div class="rate">
			<div class="rate-left">
				<div class="score">
					<span class="score-num"><Icon name="star" size={26} fill="var(--k-star)" />{rating}</span>
					<span class="score-sub">{seriesDetail.votes} community ratings</span>
				</div>
				<div class="divider"></div>
				<div>
					<div class="rate-title">Rate this series</div>
					<div class="rate-label">{ratingLabel}</div>
				</div>
			</div>
			<div class="rate-right">
				<Stars bind:value={userRating} max={10} size={24} onchange={onRate} />
				{#if userRating > 0 && !needsAuth}
					<button class="clear" onclick={clearRating}>Clear</button>
				{/if}
			</div>
		</div>
	</div>

	<!-- chapters -->
	<div class="chapters k-gutter">
		<div class="ch-head">
			<div class="ch-title">
				<h2>Chapters</h2>
				<span class="ch-total">{totalCh} total</span>
			</div>
			<div class="ch-sort">
				<button class="sortchip" class:on={sort === 'newest'} onclick={() => (sort = 'newest')}
					>Newest</button
				>
				<button class="sortchip" class:on={sort === 'oldest'} onclick={() => (sort = 'oldest')}
					>Oldest</button
				>
			</div>
		</div>
		<div class="ch-list">
			{#each chapters as c (c.id ?? c.n)}
				<a class="ch-row" class:read={c.read} href={chapterHref(c.id)}>
					<span class="ch-num">{c.n}</span>
					<div class="ch-info">
						<div class="ch-line">
							<span class="ch-name">{c.title}</span>
							{#if c.isNew}<span class="new">NEW</span>{/if}
						</div>
						<div class="ch-date">{c.date}</div>
					</div>
					{#if c.read}<span class="ch-read"
							><Icon name="check" size={13} strokeWidth={2.4} />Read</span
						>{/if}
					<Icon name="chevron-right" size={16} stroke="#57554f" />
				</a>
			{/each}
		</div>
	</div>

	<!-- related -->
	{#if relatedSeries.length}
		<div class="related k-gutter">
			<h2>Readers Also Enjoyed</h2>
			<div class="related-row">
				{#each relatedSeries as item (item.title)}
					<MangaCard
						title={item.title}
						sub={`${item.genre} · ${item.ch} ch`}
						rating={item.rating}
						cover={item.cover}
						id={item.id}
						fixed
					/>
				{/each}
			</div>
		</div>
	{/if}
{:else}
	<!-- NOT FOUND -->
	<div class="not-found k-gutter">
		<div class="nf-icon"><Icon name="alert" size={28} /></div>
		<h1>Series not found</h1>
		<p>We couldn’t load this series. It may have been removed, or the link is broken.</p>
		<div class="nf-btns">
			<a class="nf-primary" href="/browse">Browse series</a>
			<a class="nf-ghost" href="/">Go home</a>
		</div>
	</div>
{/if}

<!-- reviews -->
<div class="comments k-gutter">
	<div class="c-head">
		<h2>Reviews</h2>
		<span class="c-count">{reviews.length}</span>
	</div>

	{#if needsAuth}
		<div class="signin-prompt">
			<span>Sign in to rate this series and post a review.</span>
			<a class="signin-link" href={`/login?redirect=${encodeURIComponent(page.url.pathname)}`}
				>Sign in</a
			>
		</div>
	{:else}
		<div class="composer">
			<div class="avatar">
				<Avatar
					url={auth.user?.avatarUrl}
					name={auth.user?.username ?? 'You'}
					colorKey={auth.user?.id}
					size={40}
				/>
			</div>
			<div class="composer-body">
				<textarea
					bind:value={draft}
					placeholder={socialLive() && userRating === 0
						? 'Rate the series above, then share your review…'
						: 'Share your thoughts on this series…'}
					rows="3"
				></textarea>
				{#if postError}<p class="post-error">{postError}</p>{/if}
				<div class="composer-actions">
					<label class="spoiler-toggle">
						<input type="checkbox" bind:checked={spoiler} />
						<span>Contains spoilers</span>
					</label>
					<button class="post" onclick={postReview} disabled={postDisabled}>
						{posting ? 'Posting…' : 'Post review'}
					</button>
				</div>
			</div>
		</div>
	{/if}

	<div class="c-list">
		{#each reviews as c (c.id)}
			<div class="comment">
				<div class="avatar sm">
					<Avatar url={c.avatarUrl} name={c.name} colorKey={c.authorId} size={40} />
				</div>
				<div class="c-body">
					<div class="c-meta">
						<span class="c-name">{c.name}</span>
						{#if c.score > 0}
							<span class="c-score"
								><Icon name="star" size={12} fill="var(--k-star)" />{c.score}</span
							>
						{/if}
						{#if c.mine}<span class="c-mine">You</span>{/if}
						<span class="c-time">· {c.time}</span>
					</div>
					{#if c.hasSpoiler && !revealed[c.id]}
						<button class="spoiler-veil" onclick={() => reveal(c.id)}>
							<Icon name="alert" size={14} />Spoiler — tap to reveal
						</button>
					{:else}
						<p class="c-text">{c.body}</p>
					{/if}
					<div class="c-actions">
						<button class="like" class:on={c.liked} onclick={() => toggleLike(c.id)}>
							<Icon
								name="heart"
								size={15}
								fill={c.liked ? 'currentColor' : 'none'}
								strokeWidth={2}
							/>{c.likes}
						</button>
						<button class="reply">Reply</button>
					</div>
				</div>
			</div>
		{/each}
	</div>
</div>

<!-- discussion (series-level comments; distinct from Reviews above) -->
<div class="discussion k-gutter">
	<h2 class="discussion-title">Discussion</h2>
	{#if seriesId}
		<CommentThread
			targetType="series"
			targetId={seriesId}
			storageKey={socialKey}
			prompt="Share your thoughts on this series…"
		/>
	{/if}
</div>

<Footer />

<style>
	.discussion {
		margin-top: 40px;
		padding-bottom: 8px;
	}
	.discussion-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 22px;
		color: var(--k-text-bright);
		margin: 0 0 20px;
	}
	.backdrop {
		position: relative;
		height: 380px;
		overflow: hidden;
		background: #111112;
		background-image: repeating-linear-gradient(
			135deg,
			rgba(255, 255, 255, 0.03) 0 2px,
			transparent 2px 13px
		);
	}
	.kv-tag {
		position: absolute;
		left: var(--k-gutter);
		top: 26px;
		font-family: var(--k-font-mono);
		font-size: 10.5px;
		letter-spacing: 0.16em;
		color: rgba(255, 255, 255, 0.22);
	}
	.fade-top {
		position: absolute;
		inset: 0;
		background: linear-gradient(
			to top,
			#0c0c0d 4%,
			rgba(12, 12, 13, 0.72) 40%,
			rgba(12, 12, 13, 0.25) 100%
		);
	}
	.fade-left {
		position: absolute;
		inset: 0;
		background: linear-gradient(to right, rgba(12, 12, 13, 0.7) 0%, transparent 55%);
	}
	.hero {
		position: relative;
		margin-top: -172px;
		z-index: 5;
	}
	.hero-row {
		display: flex;
		gap: 40px;
		align-items: flex-end;
	}
	.poster {
		position: relative;
		flex: 0 0 auto;
		width: 224px;
		height: 328px;
		border-radius: 12px;
		overflow: hidden;
		border: 1px solid var(--k-border-2);
		box-shadow: 0 26px 60px rgba(0, 0, 0, 0.6);
	}
	.info {
		flex: 1;
		min-width: 0;
		padding-bottom: 8px;
	}
	/* loading skeleton */
	.poster.k-skeleton {
		border-radius: 12px;
	}
	.skel-info {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.skel-info .k-skeleton {
		border-radius: 8px;
	}
	.sk-badges {
		width: 180px;
		height: 24px;
	}
	.sk-h1 {
		width: 60%;
		max-width: 460px;
		height: 54px;
	}
	.sk-creators {
		width: 220px;
		height: 16px;
	}
	.sk-facts {
		width: 80%;
		max-width: 520px;
		height: 16px;
	}
	.sk-cta {
		width: 340px;
		height: 50px;
		margin-top: 6px;
	}
	/* not-found */
	.not-found {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 14px;
		padding: 120px 20px;
		text-align: center;
	}
	.nf-icon {
		width: 64px;
		height: 64px;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-2);
		color: var(--k-hiatus);
	}
	.not-found h1 {
		font-size: 34px;
		margin: 0;
		color: var(--k-text-bright);
	}
	.not-found p {
		margin: 0;
		max-width: 380px;
		font-size: 14.5px;
		line-height: 1.6;
		color: var(--k-text-dim);
	}
	.nf-btns {
		display: flex;
		gap: 12px;
		margin-top: 10px;
		flex-wrap: wrap;
		justify-content: center;
	}
	.nf-primary,
	.nf-ghost {
		height: 46px;
		padding: 0 24px;
		display: inline-flex;
		align-items: center;
		border-radius: 8px;
		font-weight: 700;
		font-size: 14px;
		text-decoration: none;
	}
	.nf-primary {
		background: var(--k-primary);
		color: var(--k-on-primary);
	}
	.nf-ghost {
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text);
	}
	.nf-ghost:hover {
		border-color: rgba(255, 255, 255, 0.34);
	}
	.badges {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 16px;
	}
	.badge {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		border-radius: var(--k-radius-pill);
		font-size: 11.5px;
		font-weight: 700;
	}
	.badge.status {
		padding: 5px 12px;
		background: rgba(255, 255, 255, 0.06);
		border: 1px solid var(--k-border-2);
		letter-spacing: 0.05em;
		color: var(--k-text-2);
	}
	.status-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--k-dot);
	}
	.badge.type {
		padding: 5px 13px 5px 10px;
		background: rgba(224, 131, 105, 0.12);
		border: 1px solid rgba(224, 131, 105, 0.4);
		font-weight: 800;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--k-accent);
	}
	.type-flag {
		font-size: 15px;
		line-height: 1;
	}
	h1 {
		font-size: 58px;
		line-height: 1;
		letter-spacing: -0.03em;
		margin: 0 0 8px;
		color: var(--k-text-bright);
	}
	.creators {
		font-size: 15px;
		color: var(--k-text-dim);
		margin-bottom: 22px;
	}
	.facts {
		display: flex;
		align-items: center;
		gap: 22px;
		flex-wrap: wrap;
		margin-bottom: 24px;
	}
	.fact {
		font-size: 14px;
		color: var(--k-text-muted);
	}
	.fact.rating {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-weight: 700;
		font-size: 15px;
		color: var(--k-text);
	}
	.votes {
		color: var(--k-text-faint);
		font-weight: 500;
		font-size: 13px;
	}
	.sep {
		width: 4px;
		height: 4px;
		border-radius: 50%;
		background: #403f3b;
	}
	.cta {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.read {
		height: 50px;
		padding: 0 30px;
		border: none;
		border-radius: 8px;
		background: var(--k-primary);
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 15px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 9px;
		text-decoration: none;
	}
	.read:hover {
		background: var(--k-primary-hover);
	}
	.lib {
		height: 50px;
		padding: 0 22px;
		border-radius: 8px;
		font-weight: 700;
		font-size: 14px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 9px;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text);
		transition: all 0.15s;
	}
	.lib:hover {
		border-color: rgba(255, 255, 255, 0.34);
	}
	.lib.in {
		background: rgba(95, 191, 126, 0.12);
		border-color: rgba(95, 191, 126, 0.5);
		color: #7fd39a;
	}
	.share {
		width: 50px;
		height: 50px;
		flex: 0 0 auto;
		border-radius: 8px;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-3);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.share:hover {
		border-color: rgba(255, 255, 255, 0.34);
		color: var(--k-text);
	}
	.genres-syn {
		padding-top: 44px;
		padding-bottom: 8px;
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.genre-chips {
		display: flex;
		gap: 9px;
		flex-wrap: wrap;
	}
	.gchip {
		font-size: 13px;
		color: var(--k-text-3);
		padding: 7px 15px;
		border-radius: var(--k-radius-pill);
		border: 1px solid var(--k-border-3);
	}
	.synopsis {
		max-width: 820px;
		font-size: 15.5px;
		line-height: 1.7;
		color: var(--k-text-muted);
		margin: 0;
	}
	.rate-wrap {
		padding-top: 40px;
	}
	.rate {
		display: flex;
		flex-wrap: wrap;
		gap: 28px;
		align-items: center;
		justify-content: space-between;
		border: 1px solid var(--k-border);
		border-radius: 12px;
		padding: 24px 30px;
		background: var(--k-surface);
	}
	.rate-left {
		display: flex;
		align-items: center;
		gap: 24px;
	}
	.score {
		display: flex;
		flex-direction: column;
	}
	.score-num {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 38px;
		line-height: 1;
		color: var(--k-text-bright);
	}
	.score-sub {
		font-size: 12px;
		color: var(--k-text-faint);
		margin-top: 6px;
	}
	.divider {
		width: 1px;
		height: 52px;
		background: var(--k-border-2);
	}
	.rate-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 16px;
		color: var(--k-text);
	}
	.rate-label {
		font-size: 13px;
		color: var(--k-text-dimmer);
		margin-top: 4px;
	}
	.rate-right {
		display: flex;
		align-items: center;
		gap: 16px;
	}
	.clear {
		background: none;
		border: none;
		font-size: 12.5px;
		font-weight: 700;
		color: var(--k-text-dimmer);
		cursor: pointer;
		white-space: nowrap;
	}
	.clear:hover {
		color: var(--k-text);
	}
	.chapters {
		padding-top: 48px;
		display: flex;
		flex-direction: column;
		gap: 20px;
	}
	.ch-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
		flex-wrap: wrap;
	}
	.ch-title {
		display: flex;
		align-items: baseline;
		gap: 12px;
	}
	.ch-title h2 {
		font-size: 21px;
		margin: 0;
		color: var(--k-text);
	}
	.ch-total {
		font-size: 13px;
		color: var(--k-text-faint);
	}
	.ch-sort {
		display: flex;
		gap: 8px;
	}
	.sortchip {
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
	.sortchip.on {
		background: var(--k-primary);
		color: var(--k-on-primary);
		border-color: var(--k-primary);
	}
	.ch-list {
		border: 1px solid var(--k-border);
		border-radius: 12px;
		overflow: hidden;
		max-height: 560px;
		overflow-y: auto;
	}
	.ch-row {
		display: flex;
		align-items: center;
		gap: 18px;
		padding: 15px 20px;
		border-bottom: 1px solid rgba(255, 255, 255, 0.05);
		text-decoration: none;
		transition: background 0.12s;
	}
	.ch-row:hover {
		background: rgba(255, 255, 255, 0.035);
	}
	.ch-num {
		flex: 0 0 auto;
		width: 52px;
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 16px;
		color: var(--k-text);
	}
	.ch-row.read .ch-num {
		color: var(--k-text-ghost);
	}
	.ch-info {
		flex: 1;
		min-width: 0;
	}
	.ch-line {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.ch-name {
		font-weight: 600;
		font-size: 14.5px;
		color: var(--k-text-1);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ch-row.read .ch-name {
		color: var(--k-text-dim);
	}
	.new {
		flex: 0 0 auto;
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.06em;
		color: var(--k-on-primary);
		background: var(--k-star);
		padding: 2px 7px;
		border-radius: 5px;
	}
	.ch-date {
		font-size: 12px;
		color: var(--k-text-fainter);
		margin-top: 3px;
	}
	.ch-read {
		flex: 0 0 auto;
		font-size: 11.5px;
		color: var(--k-text-ghost);
		display: inline-flex;
		align-items: center;
		gap: 5px;
	}
	.related {
		display: flex;
		flex-direction: column;
		gap: 20px;
		padding-top: 56px;
	}
	.related h2 {
		font-size: 21px;
		margin: 0;
		color: var(--k-text);
	}
	.related-row {
		display: flex;
		gap: 22px;
		overflow-x: auto;
		padding-bottom: 4px;
	}
	.comments {
		display: flex;
		flex-direction: column;
		gap: 22px;
		padding-top: 56px;
		padding-bottom: 80px;
		margin-top: 56px;
		border-top: 1px solid var(--k-border);
	}
	.c-head {
		display: flex;
		align-items: baseline;
		gap: 12px;
	}
	.c-head h2 {
		font-size: 21px;
		margin: 0;
		color: var(--k-text);
	}
	.c-count {
		font-size: 13px;
		color: var(--k-text-faint);
	}
	.composer {
		display: flex;
		gap: 14px;
		align-items: flex-start;
		max-width: 820px;
	}
	.avatar {
		flex: 0 0 auto;
		width: 40px;
		height: 40px;
		border-radius: 50%;
		background: var(--k-avatar);
		border: 1px solid var(--k-border-2);
		display: flex;
		align-items: center;
		justify-content: center;
		font-weight: 700;
		font-size: 14px;
		color: var(--k-text-3);
	}
	.avatar.sm {
		background: var(--k-surface-5);
	}
	.composer-body {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}
	textarea {
		width: 100%;
		resize: vertical;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-2);
		border-radius: 10px;
		padding: 14px 16px;
		color: var(--k-text);
		font-family: var(--k-font-sans);
		font-size: 14.5px;
		line-height: 1.55;
		outline: none;
		transition: border-color 0.15s;
	}
	textarea:focus {
		border-color: var(--k-border-strong);
	}
	.composer-actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}
	.spoiler-toggle {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-size: 12.5px;
		font-weight: 600;
		color: var(--k-text-dimmer);
		cursor: pointer;
		user-select: none;
	}
	.spoiler-toggle input {
		width: 15px;
		height: 15px;
		accent-color: var(--k-accent);
		cursor: pointer;
	}
	.post-error {
		margin: 0;
		font-size: 12.5px;
		color: var(--k-accent);
	}
	.post {
		height: 42px;
		padding: 0 22px;
		border: none;
		border-radius: 8px;
		background: var(--k-primary);
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 14px;
		cursor: pointer;
		transition: opacity 0.15s;
	}
	.post:disabled {
		opacity: 0.45;
		cursor: default;
	}
	.signin-prompt {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
		flex-wrap: wrap;
		max-width: 820px;
		padding: 18px 22px;
		background: var(--k-surface);
		border: 1px solid var(--k-border-1);
		border-radius: 12px;
		font-size: 14px;
		color: var(--k-text-dim);
	}
	.signin-link {
		flex: 0 0 auto;
		height: 38px;
		padding: 0 20px;
		display: inline-flex;
		align-items: center;
		border-radius: var(--k-radius-pill);
		background: var(--k-primary);
		color: var(--k-on-primary);
		font-size: 13px;
		font-weight: 700;
		text-decoration: none;
	}
	.signin-link:hover {
		background: var(--k-primary-hover);
	}
	.c-score {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 11.5px;
		font-weight: 800;
		color: var(--k-star);
		background: rgba(246, 183, 60, 0.12);
		border: 1px solid rgba(246, 183, 60, 0.3);
		border-radius: 5px;
		padding: 1px 7px 1px 5px;
	}
	.c-mine {
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--k-on-primary);
		background: var(--k-primary);
		border-radius: 5px;
		padding: 2px 6px;
	}
	.spoiler-veil {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		margin: 9px 0 12px;
		padding: 10px 14px;
		background: var(--k-surface-3);
		border: 1px dashed var(--k-border-3);
		border-radius: 8px;
		color: var(--k-text-dim);
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
	}
	.spoiler-veil:hover {
		color: var(--k-text);
		border-color: var(--k-border-strong);
	}
	.c-list {
		display: flex;
		flex-direction: column;
		max-width: 820px;
	}
	.comment {
		display: flex;
		gap: 14px;
		align-items: flex-start;
		padding: 22px 0;
		border-top: 1px solid var(--k-border);
	}
	.c-body {
		flex: 1;
		min-width: 0;
	}
	.c-meta {
		display: flex;
		align-items: center;
		gap: 9px;
		flex-wrap: wrap;
	}
	.c-name {
		font-weight: 700;
		font-size: 14px;
		color: var(--k-text-1);
	}
	.c-time {
		font-size: 12px;
		color: var(--k-text-fainter);
	}
	.c-text {
		font-size: 14px;
		line-height: 1.65;
		color: var(--k-text-muted);
		margin: 9px 0 12px;
	}
	.c-actions {
		display: flex;
		align-items: center;
		gap: 22px;
	}
	.like {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		background: none;
		border: none;
		padding: 0;
		font-size: 12.5px;
		font-weight: 700;
		color: var(--k-text-dimmer);
		cursor: pointer;
		transition: color 0.15s;
	}
	.like:hover {
		color: var(--k-text);
	}
	.like.on {
		color: var(--k-star);
	}
	.reply {
		background: none;
		border: none;
		font-size: 12.5px;
		font-weight: 700;
		color: var(--k-text-dimmer);
		cursor: pointer;
	}
	.reply:hover {
		color: var(--k-text);
	}
	@media (max-width: 640px) {
		.hero-row {
			gap: 20px;
		}
		.poster {
			width: 140px;
			height: 206px;
		}
		h1 {
			font-size: 34px;
		}
	}
</style>
