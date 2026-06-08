<script lang="ts">
	import { page } from '$app/state';
	import { untrack } from 'svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Stars from '$lib/components/Stars.svelte';
	import MangaCard from '$lib/components/MangaCard.svelte';
	import Cover from '$lib/components/Cover.svelte';
	import CommentThread from '$lib/components/CommentThread.svelte';
	import StatusMenu from '$lib/components/StatusMenu.svelte';
	import { FLAG, type Shelf } from '$lib/data/types';
	import { setLibraryMark, setFavorite, setLibraryStatus, getSeries } from '$lib/data/source';
	import { setPreferredTranslator } from '$lib/data/translator-pref.svelte';
	import { auth } from '$lib/auth.svelte';
	import { socialLive, loadSeriesSocial, saveSeriesRating } from '$lib/data/social-repo';

	import type { SeriesView } from '$lib/data/source';

	let { data } = $props();
	// Stream the series detail; the page shows a hero/chapter skeleton until it
	// resolves. `data.series` never rejects — it resolves to { view, error }: a null
	// view with error=false is a genuine not-found; error=true is a backend failure
	// (outage) that gets an honest error state + retry instead of "not found".
	let view = $state<SeriesView | null>(null);
	let loading = $state(true);
	let loadError = $state(false);
	$effect(() => {
		loading = true;
		view = null;
		loadError = false;
		// Guard against a slow A→B navigation letting A's response overwrite B
		// (mirrors the social-load effect below).
		let cancelled = false;
		data.series.then((r) => {
			if (cancelled) return;
			view = r.view;
			loadError = r.error;
			loading = false;
		});
		return () => {
			cancelled = true;
		};
	});

	async function retrySeries(): Promise<void> {
		loading = true;
		view = null;
		loadError = false;
		// Pin the slug we're retrying so a mid-flight navigation can't be clobbered.
		const slug = data.slug;
		const r = await getSeries(slug);
		if (data.slug !== slug) return;
		view = r.view;
		loadError = r.error;
		loading = false;
	}

	const detail = $derived(view?.detail);
	const seriesDetail = $derived(view?.detail); // alias: template reads seriesDetail.author/artist/votes/…
	const title = $derived(detail?.title ?? '');
	const type = $derived(detail?.type ?? 'Manga');
	const rating = $derived(detail?.rating ?? '');
	const totalCh = $derived(detail?.totalCh ?? 0);
	const statusLabel = $derived(detail?.statusLabel ?? '');
	const continueCh = $derived(detail?.continueCh ?? 1);
	// All-time views, compactly formatted (1.2K / 3.4M). Only shown once a series has
	// any recorded reads, so a brand-new/untracked series doesn't display "0 views".
	const viewsTotal = $derived(detail?.viewsTotal ?? 0);
	const viewsLabel = $derived(
		new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(
			viewsTotal,
		),
	);
	const relatedSeries = $derived(view?.related ?? []);

	// `src` (a Suwayomi manga id) pins which source a chapter is read from when it
	// comes from a non-preferred source in the aggregated list (S2 per-chapter
	// fallback); omitted for the preferred/mirror source.
	function chapterHref(chId?: string, src?: string | null) {
		const base = `/read/${detail?.id ?? data.slug}`;
		if (!chId) return base;
		return `${base}?ch=${chId}${src ? `&src=${src}` : ''}`;
	}
	const readHref = $derived(chapterHref(detail?.startChapterId, detail?.startChapterSrc));

	let inLibrary = $state(false);
	let marking = $state(false);
	// Favourite + explicit shelf, reflected from the backend on load then toggled
	// optimistically. Setting either implies library membership (server adds it).
	let isFavorite = $state(false);
	let favBusy = $state(false);
	let libStatus = $state<Shelf | null>(null);
	// The star rating (community score) is kept; the old bespoke review-comment list
	// was retired in favour of the unified CommentThread discussion below. `myBody`
	// is preserved so a rating upsert doesn't wipe any review body already on record.
	let userRating = $state(0);
	let myBody = $state('');
	let sort = $state<'newest' | 'oldest'>('newest');

	const seriesId = $derived(detail?.id ?? '');
	const socialKey = $derived(data.slug ?? page.params.slug ?? '');
	const needsAuth = $derived(socialLive() && !auth.user);

	// Reflect the backend's library state on load (resets when navigating series).
	$effect(() => {
		const marked = detail?.isMarked ?? false;
		const fav = detail?.isFavorite ?? false;
		const status = detail?.libraryStatus ?? null;
		untrack(() => {
			inLibrary = marked;
			isFavorite = fav;
			libStatus = status;
		});
	});

	// The shelf the picker shows: the explicit choice, else derived from progress
	// (mirrors the library/profile derivation — completed when fully read, plan when
	// untouched, otherwise reading).
	const readCount = $derived((view?.chapters ?? []).filter((c) => c.read).length);
	const effectiveShelf = $derived<Shelf>(
		libStatus ??
			(totalCh > 0 && readCount >= totalCh ? 'completed' : readCount === 0 ? 'plan' : 'reading'),
	);

	async function toggleLibrary(): Promise<void> {
		if (marking) return;
		const next = !inLibrary;
		inLibrary = next; // optimistic
		marking = true;
		try {
			inLibrary = await setLibraryMark(seriesId, next);
			// Removing from the library also drops its shelf/favourite server-side (the
			// row is deleted) — reflect that so the controls don't show stale state.
			if (!inLibrary) {
				libStatus = null;
				isFavorite = false;
			}
		} finally {
			marking = false;
		}
	}

	async function toggleFavorite(): Promise<void> {
		if (favBusy) return;
		const next = !isFavorite;
		isFavorite = next; // optimistic
		if (next) inLibrary = true; // favouriting implies membership
		favBusy = true;
		try {
			isFavorite = await setFavorite(seriesId, next);
		} finally {
			favBusy = false;
		}
	}

	async function chooseStatus(s: Shelf): Promise<void> {
		const prev = libStatus;
		libStatus = s; // optimistic
		inLibrary = true; // filing a shelf implies membership
		const result = await setLibraryStatus(seriesId, s);
		libStatus = result ?? prev;
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
			})
			.catch(() => {});
		return () => {
			cancelled = true;
		};
	});

	// Share the current series via the Web Share API, falling back to copying the
	// link to the clipboard when the native sheet isn't available.
	let shareCopied = $state(false);
	async function shareSeries(): Promise<void> {
		if (typeof window === 'undefined') return;
		const url = window.location.href;
		const shareData = {
			title: title || 'komiq',
			text: title ? `Check out ${title} on komiq` : 'Check out this series on komiq',
			url,
		};
		if (navigator.share) {
			try {
				await navigator.share(shareData);
				return;
			} catch {
				return; // user dismissed the share sheet
			}
		}
		try {
			await navigator.clipboard?.writeText(url);
			shareCopied = true;
			setTimeout(() => (shareCopied = false), 2000);
		} catch {
			/* clipboard unavailable — nothing more we can do */
		}
	}

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

	// Translators (sources) available for this work, and switching between them.
	const translators = $derived(view?.translators ?? []);
	const selectedTranslatorKey = $derived(view?.selectedTranslatorKey ?? null);
	const workId = $derived(view?.workId ?? null);
	let switching = $state(false);

	async function selectTranslator(key: string): Promise<void> {
		if (!workId || switching || key === selectedTranslatorKey) return;
		setPreferredTranslator(workId, key);
		switching = true;
		try {
			const r = await getSeries(data.slug);
			if (r.view) view = r.view;
		} finally {
			switching = false;
		}
	}

	// Credits (S2 enrichment): dedupe by name, collecting roles; fall back to the
	// single author/artist line when the work carries no structured credits.
	const ROLE_LABEL: Record<string, string> = { author: 'Story', artist: 'Art' };
	const creditList = $derived.by(() => {
		const raw = seriesDetail?.credits ?? [];
		if (raw.length) {
			const byName = new Map<string, Set<string>>();
			for (const c of raw) {
				if (!byName.has(c.name)) byName.set(c.name, new Set());
				byName.get(c.name)!.add(c.role);
			}
			return [...byName.entries()].map(([name, roles]) => ({
				name,
				label:
					roles.has('author') && roles.has('artist')
						? 'Story & Art'
						: [...roles].map((r) => ROLE_LABEL[r] ?? r).join(' · '),
			}));
		}
		const names = [seriesDetail?.author, seriesDetail?.artist].filter(
			(n): n is string => !!n && n.length > 0,
		);
		return [...new Set(names)].map((name) => ({ name, label: '' }));
	});

	// Localized descriptions (S2): default to the app-language pick; let the reader
	// switch language when the work carries more than one.
	const descriptions = $derived(seriesDetail?.descriptions ?? []);
	let descLangSel = $state<string | null>(null);
	$effect(() => {
		seriesId; // reset the language choice when navigating between series
		untrack(() => (descLangSel = null));
	});
	const synopsisText = $derived.by(() => {
		if (descLangSel) {
			const d = descriptions.find((x) => x.lang === descLangSel);
			if (d) return d.description;
		}
		return seriesDetail?.synopsis ?? '';
	});
	function langName(code: string): string {
		try {
			const dn = new Intl.DisplayNames([navigator.language || 'en'], { type: 'language' });
			return dn.of(code) ?? code.toUpperCase();
		} catch {
			return code.toUpperCase();
		}
	}

	// Cover gallery (F2): the full MangaDex cover set; the hero keeps the primary.
	const covers = $derived(seriesDetail?.covers ?? []);
	let activeCoverIdx = $state<number | null>(null);
	const activeCover = $derived(activeCoverIdx != null ? (covers[activeCoverIdx] ?? null) : null);
	function coverLabel(c: {
		volume: string | null;
		lang: string | null;
		isPrimary: boolean;
	}): string {
		const parts: string[] = [];
		if (c.volume) parts.push(`Vol. ${c.volume}`);
		if (c.lang) parts.push(c.lang.toUpperCase());
		if (c.isPrimary) parts.push('Primary');
		return parts.length ? parts.join(' · ') : 'Cover';
	}
	function openCover(i: number): void {
		activeCoverIdx = i;
	}
	function closeCover(): void {
		activeCoverIdx = null;
	}
	// Close the lightbox on Escape.
	$effect(() => {
		if (activeCoverIdx == null) return;
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') closeCover();
		};
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
	});

	const ratingLabel = $derived(
		needsAuth
			? 'Sign in to rate this series'
			: userRating > 0
				? `You rated ${userRating} / 10 · tap a star to update`
				: 'Tap a star to rate — out of 10',
	);
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
				<div class="creators">
					{#if creditList.length}
						{#each creditList as c, i (c.name)}
							{#if i > 0}<span class="cr-sep">·</span>{/if}<span class="cr-name">{c.name}</span
							>{#if c.label}<span class="cr-role">{c.label}</span>{/if}
						{/each}
					{:else}
						{seriesDetail.author} · {seriesDetail.artist}
					{/if}
				</div>
				<div class="facts">
					<span class="fact rating"
						><Icon name="star" size={16} fill="var(--k-star)" />{rating}<span class="votes"
							>({seriesDetail.votes})</span
						></span
					>
					<span class="sep"></span>
					<span class="fact">{totalCh} chapters</span>
					{#if viewsTotal > 0}
						<span class="sep"></span>
						<span class="fact"><Icon name="eye" size={15} />{viewsLabel} views</span>
					{/if}
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
					{#if inLibrary}
						<StatusMenu
							status={effectiveShelf}
							onchange={chooseStatus}
							disabled={marking}
							variant="button"
						/>
					{/if}
					<button
						class="fav"
						class:on={isFavorite}
						disabled={favBusy}
						aria-pressed={isFavorite}
						aria-label={isFavorite ? 'Remove from favourites' : 'Add to favourites'}
						title={isFavorite ? 'Favourited' : 'Add to favourites'}
						onclick={toggleFavorite}
					>
						<Icon name="heart" size={18} fill={isFavorite ? 'currentColor' : 'none'} />
					</button>
					<button
						class="share"
						class:copied={shareCopied}
						aria-label={shareCopied ? 'Link copied' : 'Share'}
						title={shareCopied ? 'Link copied' : 'Share'}
						onclick={shareSeries}><Icon name={shareCopied ? 'check' : 'share'} size={18} /></button
					>
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
		{#if descriptions.length > 1}
			<div class="desc-lang">
				<span class="desc-lang-label">Description language</span>
				<select bind:value={descLangSel} aria-label="Description language">
					<option value={null}>Default ({(seriesDetail.descLang ?? 'en').toUpperCase()})</option>
					{#each descriptions as d (d.lang)}
						<option value={d.lang}>{langName(d.lang)}</option>
					{/each}
				</select>
			</div>
		{/if}
		<p class="synopsis">{synopsisText}</p>
	</div>

	<!-- covers (F2): additional per-volume/locale covers; hidden for ≤1 cover -->
	{#if covers.length > 1}
		<div class="covers k-gutter">
			<div class="covers-head">
				<h2>Covers</h2>
				<span class="covers-count">{covers.length}</span>
			</div>
			<div class="covers-strip">
				{#each covers as c, i (c.url)}
					<button class="cover-cell" onclick={() => openCover(i)} title={coverLabel(c)}>
						<div class="cover-thumb">
							<Cover src={c.thumbnailUrl} alt={coverLabel(c)} loading="lazy" />
							{#if c.isPrimary}<span class="cover-primary">Primary</span>{/if}
						</div>
						{#if c.volume || c.lang}
							<span class="cover-meta">
								{#if c.volume}<span class="cover-vol">Vol. {c.volume}</span>{/if}
								{#if c.lang}<span class="cover-lang">{c.lang.toUpperCase()}</span>{/if}
							</span>
						{/if}
					</button>
				{/each}
			</div>
		</div>
	{/if}

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

	<!-- translators (sources) -->
	{#if translators.length}
		<div class="translators k-gutter">
			<div class="tr-head">
				<span class="tr-label">Translation source</span>
				<span class="tr-sub"
					>{translators.length === 1
						? 'Only one source available'
						: `${translators.length} sources · pick who you read from`}</span
				>
			</div>
			<div class="tr-list" class:switching>
				{#each translators as t (t.key)}
					<button
						class="tr-opt"
						class:on={t.key === selectedTranslatorKey}
						disabled={switching}
						onclick={() => selectTranslator(t.key)}
						title={t.lang ? `${t.name} · ${t.lang.toUpperCase()}` : t.name}
					>
						{#if t.iconUrl}
							<img
								class="tr-logo"
								src={t.iconUrl}
								alt=""
								loading="lazy"
								decoding="async"
								referrerpolicy="no-referrer"
							/>
						{:else}
							<span class="tr-logo tr-initial">{t.name.charAt(0).toUpperCase()}</span>
						{/if}
						<span class="tr-name">
							<span class="tr-name-main"
								>{t.name}{#if t.lang}<span class="tr-lang">{t.lang.toUpperCase()}</span>{/if}</span
							>
							<span class="tr-count"
								>{t.chapterCount} {t.chapterCount === 1 ? 'chapter' : 'chapters'}</span
							>
						</span>
						{#if t.key === selectedTranslatorKey}
							<Icon name="check" size={15} strokeWidth={2.6} />
						{/if}
					</button>
				{/each}
			</div>
		</div>
	{/if}

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
				<a class="ch-row" class:read={c.read} href={chapterHref(c.id, c.src)}>
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
				{#each relatedSeries as item (item.id ?? item.title)}
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
{:else if loadError}
	<!-- LOAD ERROR (backend outage) — distinct from a genuinely missing series -->
	<div class="not-found k-gutter">
		<div class="nf-icon"><Icon name="alert" size={28} /></div>
		<h1>Couldn’t load this series</h1>
		<p>Something went wrong reaching the server. Check your connection and try again.</p>
		<div class="nf-btns">
			<button class="nf-primary" onclick={retrySeries}>Retry</button>
			<a class="nf-ghost" href="/browse">Browse series</a>
		</div>
	</div>
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

<!-- cover lightbox (F2) -->
{#if activeCover}
	<div
		class="lightbox"
		role="dialog"
		aria-modal="true"
		aria-label={coverLabel(activeCover)}
		tabindex="-1"
	>
		<button class="lb-scrim" aria-label="Close cover" onclick={closeCover}></button>
		<div class="lb-inner">
			<div class="lb-image">
				<Cover src={activeCover.url} alt={coverLabel(activeCover)} fit="contain" />
			</div>
			<div class="lb-bar">
				<span class="lb-label">{coverLabel(activeCover)}</span>
				<button class="lb-close" onclick={closeCover} aria-label="Close"
					><Icon name="x" size={18} /></button
				>
			</div>
		</div>
	</div>
{/if}

<!-- discussion — the single unified comment engine (replies + image attachments).
     The old bespoke "Reviews" comment list was removed; the star rating above is
     the series' score, and all written discussion lives in this one thread. -->
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
		border: none;
		cursor: pointer;
		font-family: inherit;
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
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
	}
	.cr-name {
		color: var(--k-text-2);
		font-weight: 600;
	}
	.cr-role {
		font-size: 10.5px;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--k-text-faint);
		background: var(--k-hover-fill);
		border: 1px solid var(--k-border-3);
		border-radius: 5px;
		padding: 1px 6px;
		margin-left: 5px;
	}
	.cr-sep {
		color: var(--k-text-disabled);
	}
	.desc-lang {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.desc-lang-label {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.09em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	.desc-lang select {
		height: 34px;
		padding: 0 10px;
		border-radius: 8px;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
		color: var(--k-text-2);
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
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
	.share.copied {
		border-color: rgba(95, 191, 126, 0.5);
		color: #7fd39a;
	}
	.fav {
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
		transition: all 0.15s;
	}
	.fav:hover:not(:disabled) {
		border-color: rgba(233, 110, 110, 0.5);
		color: #e96e6e;
	}
	.fav.on {
		background: rgba(233, 110, 110, 0.12);
		border-color: rgba(233, 110, 110, 0.5);
		color: #e96e6e;
	}
	.fav:disabled {
		opacity: 0.6;
		cursor: default;
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
	/* covers (F2) */
	.covers {
		padding-top: 44px;
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.covers-head {
		display: flex;
		align-items: baseline;
		gap: 12px;
	}
	.covers-head h2 {
		font-size: 21px;
		margin: 0;
		color: var(--k-text);
	}
	.covers-count {
		font-size: 13px;
		color: var(--k-text-faint);
	}
	.covers-strip {
		display: flex;
		gap: 14px;
		overflow-x: auto;
		padding-bottom: 8px;
		scroll-snap-type: x proximity;
	}
	.cover-cell {
		flex: 0 0 auto;
		width: 120px;
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 0;
		background: none;
		border: none;
		cursor: pointer;
		text-align: left;
		scroll-snap-align: start;
	}
	.cover-thumb {
		position: relative;
		width: 120px;
		height: 180px;
		border-radius: 9px;
		overflow: hidden;
		border: 1px solid var(--k-border-2);
		background: var(--k-surface-2);
		transition: border-color 0.15s;
	}
	.cover-cell:hover .cover-thumb {
		border-color: var(--k-border-strong);
	}
	.cover-primary {
		position: absolute;
		top: 7px;
		left: 7px;
		z-index: 1;
		font-size: 9.5px;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--k-on-primary);
		background: var(--k-primary);
		border-radius: 5px;
		padding: 2px 6px;
	}
	.cover-meta {
		display: flex;
		align-items: center;
		gap: 7px;
		font-size: 12px;
	}
	.cover-vol {
		font-weight: 700;
		color: var(--k-text-2);
	}
	.cover-lang {
		font-size: 9.5px;
		font-weight: 800;
		letter-spacing: 0.04em;
		color: var(--k-text-faint);
		background: var(--k-hover-fill);
		border: 1px solid var(--k-border-3);
		border-radius: 4px;
		padding: 1px 5px;
	}
	/* cover lightbox */
	.lightbox {
		position: fixed;
		inset: 0;
		z-index: 200;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 40px;
	}
	.lb-scrim {
		position: fixed;
		inset: 0;
		border: none;
		background: rgba(6, 6, 7, 0.86);
		backdrop-filter: blur(6px);
		cursor: zoom-out;
	}
	.lb-inner {
		position: relative;
		z-index: 1;
		display: flex;
		flex-direction: column;
		gap: 14px;
		max-width: min(90vw, 560px);
		max-height: 90vh;
	}
	.lb-image {
		position: relative;
		width: 100%;
		flex: 1 1 auto;
		min-height: 0;
		aspect-ratio: 2 / 3;
		border-radius: 12px;
		overflow: hidden;
		box-shadow: 0 30px 80px rgba(0, 0, 0, 0.7);
	}
	.lb-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 14px;
	}
	.lb-label {
		font-size: 14px;
		font-weight: 700;
		color: var(--k-text-bright);
	}
	.lb-close {
		width: 40px;
		height: 40px;
		flex: 0 0 auto;
		border-radius: 9px;
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-3);
		color: var(--k-text);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.lb-close:hover {
		border-color: var(--k-border-strong);
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
	.translators {
		padding-top: 44px;
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.tr-head {
		display: flex;
		align-items: baseline;
		gap: 12px;
		flex-wrap: wrap;
	}
	.tr-label {
		font-size: 21px;
		font-weight: 700;
		font-family: var(--k-font-display);
		color: var(--k-text);
	}
	.tr-sub {
		font-size: 13px;
		color: var(--k-text-faint);
	}
	.tr-list {
		display: flex;
		flex-wrap: wrap;
		gap: 12px;
	}
	.tr-list.switching {
		opacity: 0.6;
		pointer-events: none;
	}
	.tr-opt {
		display: inline-flex;
		align-items: center;
		gap: 12px;
		padding: 10px 16px 10px 12px;
		border-radius: 12px;
		background: var(--k-surface);
		border: 1px solid var(--k-border-2);
		color: var(--k-text-2);
		cursor: pointer;
		transition: all 0.15s;
		text-align: left;
	}
	.tr-opt:hover {
		border-color: var(--k-border-strong);
	}
	.tr-opt.on {
		border-color: var(--k-primary);
		background: rgba(224, 131, 105, 0.1);
		color: var(--k-text-bright);
	}
	.tr-logo {
		width: 34px;
		height: 34px;
		flex: 0 0 auto;
		border-radius: 8px;
		object-fit: cover;
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-2);
	}
	.tr-initial {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		font-size: 16px;
		font-weight: 800;
		color: var(--k-text-2);
	}
	.tr-name {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}
	.tr-name-main {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		font-size: 14px;
		font-weight: 700;
	}
	.tr-lang {
		font-size: 9.5px;
		font-weight: 800;
		letter-spacing: 0.04em;
		color: var(--k-text-3);
		background: var(--k-hover-fill);
		border: 1px solid var(--k-border-3);
		border-radius: 4px;
		padding: 1px 5px;
	}
	.tr-count {
		font-size: 12px;
		color: var(--k-text-faint);
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
	@media (max-width: 640px) {
		/* Shorter backdrop + matching hero pull-up so the fold isn't dominated by art. */
		.backdrop {
			height: 260px;
		}
		.hero {
			margin-top: -120px;
		}
		/* Stack the cover above full-width metadata so badges/title/facts never clip. */
		.hero-row {
			flex-direction: column;
			align-items: flex-start;
			gap: 18px;
		}
		.poster {
			width: 132px;
			height: 194px;
		}
		.info {
			width: 100%;
			padding-bottom: 0;
		}
		.badges {
			flex-wrap: wrap;
		}
		h1 {
			font-size: 30px;
		}
		.creators {
			font-size: 13.5px;
		}
		/* CTA row wraps so Continue / Library / Share never overflow; primary grows. */
		.cta {
			flex-wrap: wrap;
		}
		.read {
			flex: 1 1 auto;
			justify-content: center;
			min-width: 160px;
		}
		/* Give the translator picker + chapter rows comfortable one-handed targets. */
		.tr-opt {
			flex: 1 1 100%;
		}
		.covers-strip {
			gap: 12px;
		}
		.cover-cell,
		.cover-thumb {
			width: 104px;
		}
		.cover-thumb {
			height: 156px;
		}
		/* Rate panel stacks so the stars aren't cramped beside the score. */
		.rate {
			padding: 20px;
		}
		.rate-left {
			flex-wrap: wrap;
			gap: 16px;
		}
	}
</style>
