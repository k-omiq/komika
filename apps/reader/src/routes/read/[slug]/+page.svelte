<script lang="ts">
	import { goto } from '$app/navigation';
	import { untrack } from 'svelte';
	import { getRating, setRating } from '$lib/data/social';
	import { saveProgress } from '$lib/data/source';
	import { auth } from '$lib/auth.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Stars from '$lib/components/Stars.svelte';
	import CommentThread from '$lib/components/CommentThread.svelte';

	let { data } = $props();

	const seriesTitle = $derived(data.seriesTitle);
	const seriesHref = $derived(`/series/${data.seriesId}`);
	const pages = $derived(data.pages);
	const total = $derived(data.pages.length);
	const chNum = $derived(data.chNum);
	const chTitle = $derived(data.chTitle);
	const nextChapter = $derived(data.chapters.find((c) => c.id === data.nextChapterId));

	let mode = $state<'strip' | 'page'>('strip');
	let pageIdx = $state(0);
	let width = $state(720);
	const hasPages = $derived(total > 0);
	let chromeVisible = $state(true);
	let lockChrome = $state(false);
	let settingsOpen = $state(false);
	let chapterListOpen = $state(false);
	let progressPct = $state(0);
	let scrolledPage = $state(1);
	let userRating = $state(0);

	const chKey = $derived(`${data.seriesId}:${data.chNum}`);

	// Per-chapter rating (1–5) stays local — the backend has no chapter-rating field.
	$effect(() => {
		const k = chKey;
		untrack(() => (userRating = getRating('chapter', k)));
	});
	$effect(() => {
		const r = userRating;
		setRating(
			'chapter',
			untrack(() => chKey),
			r,
		);
	});

	let lastY = 0;
	let restored = false;
	let readMarked = $state(false);

	// Reset page + scroll position whenever the loaded chapter changes.
	$effect(() => {
		data.chapterId;
		untrack(() => {
			pageIdx = 0;
			scrolledPage = 1;
			readMarked = false;
			pageAspect = {};
		});
	});

	// The current 0-indexed page, however the chapter is being read.
	function currentPageIndex(): number {
		return mode === 'strip' ? Math.max(0, scrolledPage - 1) : pageIdx;
	}

	// Mark the chapter read once the reader reaches the end (idempotent per chapter).
	function maybeMarkRead(): void {
		if (readMarked || !data.chapterId) return;
		const atEnd = mode === 'strip' ? progressPct >= 98 : pageIdx >= total - 1;
		if (atEnd) {
			readMarked = true;
			void saveProgress(data.chapterId, Math.max(0, total - 1), true);
		}
	}

	const isStrip = $derived(mode === 'strip');
	const chromeHidden = $derived(!chromeVisible && !lockChrome);
	const currentPage = $derived(pages[Math.min(pageIdx, total - 1)]);
	const pageLabel = $derived(`${Math.min(pageIdx, total - 1) + 1} / ${total}`);
	const readout = $derived(
		isStrip
			? `Page ${scrolledPage} of ${total} · ${Math.round(progressPct)}%`
			: `Page ${pageLabel}`,
	);
	const rateLabel = $derived(
		userRating > 0 ? `You rated this chapter ${userRating}/5` : 'Enjoyed this chapter? Rate it',
	);

	function persist() {
		try {
			localStorage.setItem('yomu-reader', JSON.stringify({ mode, width }));
		} catch {
			/* ignore */
		}
	}

	function openChapter(id?: string) {
		chapterListOpen = false;
		if (!id) return;
		// Persist how far we got in the chapter we're leaving.
		void saveProgress(data.chapterId, currentPageIndex(), readMarked);
		goto(`/read/${data.seriesId}?ch=${id}`);
		const reduce =
			typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;
		window.scrollTo({ top: 0, behavior: reduce ? 'auto' : 'smooth' });
	}

	$effect(() => {
		if (!restored) {
			restored = true;
			try {
				const saved = JSON.parse(localStorage.getItem('yomu-reader') || '{}');
				if (saved) {
					mode = saved.mode || 'strip';
					width = saved.width || 720;
				}
			} catch {
				/* ignore */
			}
		}
		// Coalesce scroll work into one rAF per frame — reading layout + writing
		// reactive state on every scroll event is a jank source on fast scrolls.
		let ticking = false;
		const update = () => {
			ticking = false;
			const doc = document.documentElement;
			const max = doc.scrollHeight - window.innerHeight;
			const y = window.scrollY;
			const pct = max > 0 ? Math.min(100, Math.max(0, (y / max) * 100)) : 0;
			progressPct = pct;
			scrolledPage = Math.min(total, Math.max(1, Math.round((pct / 100) * (total - 1)) + 1));
			maybeMarkRead();
			if (!lockChrome) {
				const dy = y - lastY;
				if (y < 80) chromeVisible = true;
				else if (dy > 6) chromeVisible = false;
				else if (dy < -6) chromeVisible = true;
			} else {
				chromeVisible = true;
			}
			lastY = y;
		};
		const onScroll = () => {
			if (ticking) return;
			ticking = true;
			requestAnimationFrame(update);
		};
		window.addEventListener('scroll', onScroll, { passive: true });
		update();
		return () => window.removeEventListener('scroll', onScroll);
	});

	// ---- long-strip rendering ----------------------------------------------
	// A chapter can be 37+ full-size images. We DON'T hand-virtualize with an
	// IntersectionObserver — mounting/unmounting <img> on scroll thrashes the DOM
	// and is what caused the visible scroll bumps. Instead every page stays in the
	// DOM and we lean on native `loading="lazy"`, so off-screen images aren't
	// fetched/decoded and memory stays bounded with zero scroll-time JS. Each cell
	// reserves its space from the image's REAL aspect-ratio (measured on load) so
	// pages neither letterbox (gaps) nor shift once loaded.
	let pageAspect = $state<Record<number, string>>({});
	// A neutral manga default (~2:3) used only until a page's real ratio is known.
	const DEFAULT_ASPECT = '800 / 1200';

	function onImgLoad(e: Event, index: number): void {
		const img = e.currentTarget as HTMLImageElement;
		if (img.naturalWidth > 0 && img.naturalHeight > 0) {
			pageAspect = { ...pageAspect, [index]: `${img.naturalWidth} / ${img.naturalHeight}` };
		}
	}

	const chapterMenu = $derived(
		data.chapters.map((c) => ({
			id: c.id,
			n: c.n,
			title: c.title,
			current: c.id === data.chapterId,
		})),
	);

	function stageClick() {
		if (!lockChrome) chromeVisible = !chromeVisible;
	}
	function setMode(m: 'strip' | 'page') {
		mode = m;
		pageIdx = 0;
		persist();
	}
	function nextPage() {
		pageIdx = Math.min(total - 1, pageIdx + 1);
		maybeMarkRead();
		persist();
	}
	function prevPage() {
		pageIdx = Math.max(0, pageIdx - 1);
		persist();
	}

	// Keyboard operability: arrows/space page through single-page mode, Escape
	// closes any open overlay. Ignore when typing in the comment box.
	function onKeydown(e: KeyboardEvent) {
		const el = e.target as HTMLElement | null;
		if (el && (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT')) return;
		if (e.key === 'Escape') {
			if (settingsOpen || chapterListOpen) {
				e.preventDefault();
				settingsOpen = false;
				chapterListOpen = false;
			}
			return;
		}
		if (mode !== 'page') return;
		if (e.key === 'ArrowRight' || e.key === 'PageDown' || e.key === ' ') {
			e.preventDefault();
			nextPage();
		} else if (e.key === 'ArrowLeft' || e.key === 'PageUp') {
			e.preventDefault();
			prevPage();
		}
	}
</script>

<svelte:window onkeydown={onKeydown} />

<!-- progress bar -->
<div class="progress"><div class="progress-fill" style="width:{progressPct}%"></div></div>

<!-- top chrome -->
<header class="chrome top" class:hidden={chromeHidden}>
	<div class="top-left">
		<a class="back" href={seriesHref} aria-label="Back to series"
			><Icon name="chevron-left" size={18} /></a
		>
		<div class="titlebox">
			<a class="series" href={seriesHref}>{seriesTitle}</a>
			<div class="chsub">Ch. {chNum} · {chTitle}</div>
		</div>
	</div>
	<div class="top-right">
		<div class="ch-select">
			<button
				class="ch-btn"
				aria-haspopup="menu"
				aria-expanded={chapterListOpen}
				onclick={() => {
					chapterListOpen = !chapterListOpen;
					settingsOpen = false;
				}}
			>
				Chapter {chNum}<Icon name="chevron-down" size={14} />
			</button>
			{#if chapterListOpen}
				<div class="ch-menu">
					{#each chapterMenu as c (c.id ?? c.n)}
						<button class="ch-item" class:current={c.current} onclick={() => openChapter(c.id)}>
							<div class="ch-item-info">
								<div class="ch-item-title">Ch. {c.n} · {c.title}</div>
							</div>
							{#if c.current}<span class="reading-tag">READING</span>{/if}
						</button>
					{/each}
				</div>
			{/if}
		</div>
		<button
			class="settings-btn"
			class:on={settingsOpen}
			aria-label="Reader settings"
			aria-haspopup="dialog"
			aria-expanded={settingsOpen}
			onclick={() => {
				settingsOpen = !settingsOpen;
				chapterListOpen = false;
			}}
		>
			<Icon name="sliders" size={18} />
		</button>
	</div>
</header>

<!-- settings panel -->
{#if settingsOpen}
	<button class="settings-scrim" aria-label="Close settings" onclick={() => (settingsOpen = false)}
	></button>
	<div class="settings">
		<div class="settings-title">Reader settings</div>
		<div class="setting">
			<span class="setting-label">Reading mode</span>
			<div class="mode-btns">
				<button class="mode" class:on={mode === 'strip'} onclick={() => setMode('strip')}
					>Long strip</button
				>
				<button class="mode" class:on={mode === 'page'} onclick={() => setMode('page')}
					>Single page</button
				>
			</div>
		</div>
		<div class="setting">
			<div class="setting-row">
				<span class="setting-label">Page width</span>
				<span class="setting-val">{width}px</span>
			</div>
			<input type="range" min="480" max="1000" step="20" bind:value={width} oninput={persist} />
		</div>
		<div class="setting toggle-row">
			<span class="toggle-label">Keep chrome visible</span>
			<button
				class="toggle"
				class:on={lockChrome}
				aria-label="Toggle chrome lock"
				onclick={() => {
					lockChrome = !lockChrome;
					chromeVisible = true;
				}}
			>
				<span class="knob"></span>
			</button>
		</div>
	</div>
{/if}

<!-- Tap/keyboard target that toggles the reader chrome. A real <button> keeps
     this operable by keyboard and screen readers (the reading <main> stays a
     landmark, not an interactive element). It sits behind the page content so
     links/paging zones still take clicks. -->
<button class="chrome-toggle" aria-label="Toggle reader controls" onclick={stageClick}></button>

<!-- reading area -->
<main style="--w:{width}px">
	<div class="chapter-header">
		<div class="chapter-kicker">CHAPTER {chNum}</div>
		<h1>{chTitle}</h1>
	</div>

	{#if !hasPages}
		<div class="no-pages">
			<div class="no-pages-icon"><Icon name="alert" size={26} /></div>
			<h2>No pages available for this chapter</h2>
			<p>
				This chapter has no readable pages yet — often a licensing gap at the source. Try another
				chapter or head back to the series.
			</p>
			<div class="no-pages-btns">
				{#if data.nextChapterId}
					<button class="next-ch" onclick={() => openChapter(data.nextChapterId)}
						>Next Chapter<Icon name="arrow-right" size={16} strokeWidth={2.4} /></button
					>
				{/if}
				<a class="all-ch" href={seriesHref}><Icon name="list" size={16} />Back to series</a>
			</div>
		</div>
	{:else if isStrip}
		<div class="strip">
			{#each pages as p (p.index)}
				<div
					class="strip-cell"
					style="aspect-ratio:{pageAspect[p.index] ?? DEFAULT_ASPECT};max-width:{width}px"
				>
					{#if p.url}
						<img
							class="strip-img"
							src={p.url}
							alt={`Page ${p.label}`}
							loading="lazy"
							decoding="async"
							onload={(e) => onImgLoad(e, p.index)}
						/>
					{:else}
						<div class="strip-ph k-cover">
							<span class="page-tag">PAGE {p.label} · {p.dim}</span>
							<span class="panel-tag">MANGA PANEL</span>
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<div class="paged">
			<div
				class="paged-view"
				style={currentPage?.url ? 'height:74vh' : `height:74vh;aspect-ratio:${currentPage?.ratio}`}
			>
				{#if currentPage?.url}
					<img
						class="paged-img"
						src={currentPage.url}
						alt={`Page ${currentPage.label}`}
						decoding="async"
					/>
				{:else}
					<div class="paged-ph k-cover">
						<span class="page-tag">PAGE {currentPage?.label} · {currentPage?.dim}</span>
						<span class="panel-tag">MANGA PANEL</span>
					</div>
				{/if}
				<button
					class="zone left"
					aria-label="Previous page"
					onclick={(e) => {
						e.stopPropagation();
						prevPage();
					}}
				></button>
				<button
					class="zone right"
					aria-label="Next page"
					onclick={(e) => {
						e.stopPropagation();
						nextPage();
					}}
				></button>
			</div>
			<div class="paged-nav">
				<button class="pg-btn" onclick={prevPage}>← Prev</button>
				<span class="pg-label">{pageLabel}</span>
				<button class="pg-btn" onclick={nextPage}>Next →</button>
			</div>
		</div>
	{/if}

	<!-- end of chapter -->
	{#if hasPages}
		<div class="eoc">
			<div class="eoc-check"><Icon name="check" size={24} strokeWidth={2.4} /></div>
			<div>
				<div class="eoc-kicker">END OF CHAPTER {chNum}</div>
				<h2>{chTitle}</h2>
			</div>
			<div class="eoc-rate">
				<span class="eoc-rate-label">{rateLabel}</span>
				<Stars bind:value={userRating} max={5} size={30} gap={4} />
			</div>
			<div class="eoc-btns">
				<button
					class="next-ch"
					onclick={() => openChapter(data.nextChapterId)}
					disabled={!data.nextChapterId}
					>Next Chapter<Icon name="arrow-right" size={16} strokeWidth={2.4} /></button
				>
				<a class="all-ch" href={seriesHref}><Icon name="list" size={16} />All chapters</a>
			</div>
			{#if nextChapter}
				<div class="upnext">
					<span class="upnext-label">Up next</span>
					<button class="upnext-card" onclick={() => openChapter(nextChapter.id)}>
						<div class="upnext-cover k-cover"></div>
						<div class="upnext-info">
							<div class="upnext-title">Ch. {nextChapter.n} · {nextChapter.title}</div>
							<div class="upnext-sub">{seriesTitle}</div>
						</div>
						<Icon name="chevron-right" size={18} stroke="#57554f" />
					</button>
				</div>
			{/if}
		</div>
	{/if}

	<!-- comments (shared thread component; series discussion uses the same one) -->
	<section class="comments">
		<CommentThread
			targetType="chapter"
			targetId={data.chapterId ?? ''}
			storageKey={chKey}
			prompt="Share your thoughts on this chapter…"
		/>
	</section>
</main>

<!-- bottom chrome -->
<div class="chrome bottom" class:hidden={chromeHidden}>
	<button
		class="nav-ch"
		onclick={() => openChapter(data.prevChapterId)}
		disabled={!data.prevChapterId}><Icon name="chevron-left" size={15} />Prev</button
	>
	<span class="readout">{readout}</span>
	<button
		class="nav-ch"
		onclick={() => openChapter(data.nextChapterId)}
		disabled={!data.nextChapterId}>Next<Icon name="chevron-right" size={15} /></button
	>
</div>

<style>
	.progress {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		height: 3px;
		z-index: 60;
		background: var(--k-border);
	}
	.progress-fill {
		height: 100%;
		background: var(--k-primary);
		transition: width 0.12s linear;
	}
	.chrome {
		position: fixed;
		left: 0;
		right: 0;
		z-index: 50;
		height: 60px;
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0 24px;
		background: var(--k-glass-strong);
		backdrop-filter: blur(16px);
		transition: transform 0.28s cubic-bezier(0.4, 0, 0.2, 1);
	}
	.chrome.top {
		top: 0;
		border-bottom: 1px solid var(--k-border);
	}
	.chrome.bottom {
		bottom: 0;
		border-top: 1px solid var(--k-border);
	}
	.chrome.top.hidden {
		transform: translateY(-100%);
	}
	.chrome.bottom.hidden {
		transform: translateY(100%);
	}
	.top-left {
		display: flex;
		align-items: center;
		gap: 18px;
		min-width: 0;
	}
	.back {
		flex: 0 0 auto;
		width: 38px;
		height: 38px;
		border-radius: 9px;
		background: transparent;
		border: 1px solid var(--k-border-3);
		display: flex;
		align-items: center;
		justify-content: center;
		color: var(--k-text-3);
		text-decoration: none;
	}
	.back:hover {
		border-color: rgba(255, 255, 255, 0.34);
		color: var(--k-text);
	}
	.titlebox {
		min-width: 0;
	}
	.series {
		display: block;
		font-weight: 700;
		font-size: 14.5px;
		color: var(--k-text);
		text-decoration: none;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.chsub {
		font-size: 12px;
		color: var(--k-text-faint);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.top-right {
		display: flex;
		align-items: center;
		gap: 10px;
		flex: 0 0 auto;
	}
	.ch-select {
		position: relative;
	}
	.ch-btn {
		display: inline-flex;
		align-items: center;
		gap: 9px;
		height: 38px;
		padding: 0 14px;
		border-radius: 9px;
		background: var(--k-surface-4);
		border: 1px solid var(--k-border-2);
		color: var(--k-text-1);
		font-weight: 700;
		font-size: 13px;
		cursor: pointer;
	}
	.ch-btn:hover {
		border-color: var(--k-border-strong);
	}
	.ch-menu {
		position: absolute;
		top: 46px;
		right: 0;
		width: 280px;
		max-height: 360px;
		overflow-y: auto;
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-3);
		border-radius: 12px;
		box-shadow: 0 26px 70px rgba(0, 0, 0, 0.6);
		padding: 6px;
		z-index: 70;
	}
	.ch-item {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		padding: 10px 12px;
		border-radius: 8px;
		cursor: pointer;
		background: transparent;
		border: none;
		text-align: left;
	}
	.ch-item:hover {
		background: rgba(255, 255, 255, 0.05);
	}
	.ch-item.current {
		background: rgba(255, 255, 255, 0.05);
	}
	.ch-item-title {
		font-weight: 700;
		font-size: 13px;
		color: var(--k-text-2);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ch-item.current .ch-item-title {
		color: var(--k-text-bright);
	}
	.reading-tag {
		flex: 0 0 auto;
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.05em;
		color: var(--k-on-primary);
		background: var(--k-primary);
		padding: 2px 7px;
		border-radius: 5px;
	}
	.settings-btn {
		width: 38px;
		height: 38px;
		flex: 0 0 auto;
		border-radius: 9px;
		background: transparent;
		border: 1px solid var(--k-border-3);
		color: var(--k-text-3);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.settings-btn.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.settings-scrim {
		position: fixed;
		inset: 0;
		z-index: 55;
		border: none;
		background: rgba(8, 8, 9, 0.4);
		cursor: default;
	}
	.settings {
		position: fixed;
		z-index: 56;
		top: 68px;
		right: 24px;
		width: 300px;
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-3);
		border-radius: 16px;
		box-shadow: 0 26px 70px rgba(0, 0, 0, 0.6);
		padding: 22px;
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.settings-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 15px;
		color: var(--k-text);
	}
	.setting {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.setting-label {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.09em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	.setting-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.setting-val {
		font-size: 12px;
		font-weight: 700;
		color: var(--k-text-3);
	}
	.mode-btns {
		display: flex;
		gap: 8px;
	}
	.mode {
		flex: 1;
		height: 40px;
		border-radius: 9px;
		font-weight: 700;
		font-size: 13px;
		cursor: pointer;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-3);
		transition: all 0.15s;
	}
	.mode.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.setting input[type='range'] {
		width: 100%;
		cursor: pointer;
	}
	.toggle-row {
		flex-direction: row;
		align-items: center;
		justify-content: space-between;
		padding-top: 4px;
		border-top: 1px solid var(--k-border);
	}
	.toggle-label {
		font-size: 13px;
		color: var(--k-text-2);
		font-weight: 600;
	}
	.toggle {
		width: 44px;
		height: 26px;
		border-radius: 999px;
		border: none;
		cursor: pointer;
		padding: 3px;
		display: flex;
		justify-content: flex-start;
		background: var(--k-border-4);
		transition: background 0.18s;
	}
	.toggle.on {
		justify-content: flex-end;
		background: var(--k-ongoing);
	}
	.knob {
		width: 20px;
		height: 20px;
		border-radius: 50%;
		background: var(--k-bg);
	}
	main {
		position: relative;
		z-index: 1;
		min-height: 100vh;
		padding: 60px 0 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		pointer-events: none;
	}
	/* Re-enable pointer events on interactive controls and readable text, so the
	   chrome-toggle button underneath only receives taps on the page images,
	   gaps and margins (preserving tap-anywhere-to-toggle in strip mode) while
	   links work and comment/synopsis text stays selectable. */
	main :global(a),
	main :global(button),
	main :global(input),
	main :global(textarea),
	main :global(label),
	main :global(p),
	main :global(h1),
	main :global(h2),
	main :global(h3),
	main :global(.c-text),
	main :global(.c-name),
	main :global(.c-time) {
		pointer-events: auto;
	}
	.chrome-toggle {
		position: fixed;
		inset: 0;
		z-index: 0;
		width: 100%;
		height: 100%;
		border: none;
		background: transparent;
		cursor: pointer;
		padding: 0;
	}
	.chapter-header {
		width: 100%;
		max-width: var(--w);
		padding: 32px 20px 22px;
		text-align: center;
	}
	.chapter-kicker {
		font-size: 12px;
		letter-spacing: 0.2em;
		color: var(--k-text-disabled);
		margin-bottom: 10px;
	}
	.chapter-header h1 {
		font-size: 30px;
		letter-spacing: -0.02em;
		margin: 0;
		color: var(--k-text-bright);
	}
	.no-pages {
		width: 100%;
		max-width: 480px;
		margin: 40px 0;
		padding: 40px 24px;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 14px;
		text-align: center;
	}
	.no-pages-icon {
		width: 60px;
		height: 60px;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-2);
		color: var(--k-hiatus);
	}
	.no-pages h2 {
		font-size: 22px;
		letter-spacing: -0.02em;
		margin: 0;
		color: var(--k-text-bright);
	}
	.no-pages p {
		margin: 0;
		max-width: 360px;
		font-size: 14px;
		line-height: 1.6;
		color: var(--k-text-dim);
	}
	.no-pages-btns {
		display: flex;
		gap: 12px;
		flex-wrap: wrap;
		justify-content: center;
		margin-top: 10px;
	}
	.strip {
		display: flex;
		flex-direction: column;
		align-items: center;
		width: 100%;
	}
	/* Each cell reserves its page's space via the image's real aspect-ratio, so
	   scroll geometry stays stable while offscreen images are torn down. Pages
	   stack seamlessly — no borders/margins between them. */
	.strip-cell {
		position: relative;
		display: block;
		width: 100%;
		background: var(--k-bg);
	}
	.strip-ph {
		position: absolute;
		inset: 0;
		background-image: repeating-linear-gradient(
			135deg,
			rgba(255, 255, 255, 0.028) 0 2px,
			transparent 2px 12px
		);
	}
	.strip-img {
		display: block;
		width: 100%;
		height: 100%;
		object-fit: contain;
	}
	.page-tag {
		position: absolute;
		top: 14px;
		left: 14px;
		font-family: var(--k-font-mono);
		font-size: 10.5px;
		letter-spacing: 0.16em;
		color: #54545c;
	}
	.panel-tag {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		font-family: var(--k-font-mono);
		font-size: 11px;
		letter-spacing: 0.24em;
		color: #33333a;
	}
	.paged {
		width: 100%;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 16px;
		padding: 4px 0 20px;
	}
	.paged-view {
		position: relative;
		width: 100%;
		height: 74vh;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.paged-img {
		height: 100%;
		width: auto;
		max-width: 100%;
		object-fit: contain;
		border-radius: 6px;
	}
	.paged-ph {
		position: relative;
		height: 100%;
		aspect-ratio: 800 / 1200;
		border: 1px solid var(--k-border);
		border-radius: 6px;
		background-image: repeating-linear-gradient(
			135deg,
			rgba(255, 255, 255, 0.028) 0 2px,
			transparent 2px 12px
		);
	}
	.zone {
		position: absolute;
		top: 0;
		bottom: 0;
		width: 38%;
		background: transparent;
		border: none;
		cursor: pointer;
	}
	.zone.left {
		left: 0;
		cursor: w-resize;
	}
	.zone.right {
		right: 0;
		cursor: e-resize;
	}
	.paged-nav {
		display: flex;
		align-items: center;
		gap: 20px;
	}
	.pg-btn {
		height: 36px;
		padding: 0 16px;
		border-radius: 8px;
		background: var(--k-surface-4);
		border: 1px solid var(--k-border-2);
		color: var(--k-text-1);
		font-weight: 700;
		font-size: 13px;
		cursor: pointer;
	}
	.pg-btn:hover {
		border-color: var(--k-border-strong);
	}
	.pg-label {
		font-family: var(--k-font-display);
		font-weight: 700;
		color: var(--k-text);
	}
	.eoc {
		width: 100%;
		max-width: 640px;
		padding: 64px 20px 96px;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 26px;
		text-align: center;
	}
	.eoc-check {
		width: 52px;
		height: 52px;
		border-radius: 50%;
		background: rgba(95, 191, 126, 0.14);
		border: 1px solid rgba(95, 191, 126, 0.4);
		display: flex;
		align-items: center;
		justify-content: center;
		color: #7fd39a;
	}
	.eoc-kicker {
		font-size: 13px;
		letter-spacing: 0.14em;
		color: var(--k-text-disabled);
		margin-bottom: 8px;
	}
	.eoc h2 {
		font-size: 26px;
		letter-spacing: -0.02em;
		margin: 0;
		color: var(--k-text-bright);
	}
	.eoc-rate {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 10px;
	}
	.eoc-rate-label {
		font-size: 13px;
		color: var(--k-text-dimmer);
	}
	.eoc-btns {
		display: flex;
		gap: 12px;
		flex-wrap: wrap;
		justify-content: center;
		margin-top: 4px;
	}
	.next-ch {
		height: 50px;
		padding: 0 28px;
		border: none;
		border-radius: 9px;
		background: var(--k-primary);
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 15px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 9px;
	}
	.all-ch {
		height: 50px;
		padding: 0 24px;
		border-radius: 9px;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text);
		font-weight: 700;
		font-size: 14px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 9px;
		text-decoration: none;
	}
	.all-ch:hover {
		border-color: rgba(255, 255, 255, 0.34);
	}
	.upnext {
		width: 100%;
		margin-top: 22px;
		padding-top: 26px;
		border-top: 1px solid var(--k-border);
		display: flex;
		flex-direction: column;
		gap: 14px;
	}
	.upnext-label {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.09em;
		text-transform: uppercase;
		color: var(--k-text-faint);
		text-align: left;
	}
	.upnext-card {
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 14px;
		border-radius: 12px;
		border: 1px solid var(--k-border-1);
		background: var(--k-surface);
		cursor: pointer;
		text-align: left;
		transition: all 0.15s;
	}
	.upnext-card:hover {
		border-color: rgba(255, 255, 255, 0.22);
		background: var(--k-surface-2);
	}
	.upnext-cover {
		flex: 0 0 auto;
		width: 52px;
		height: 74px;
		border-radius: 7px;
	}
	.upnext-info {
		flex: 1;
		min-width: 0;
	}
	.upnext-title {
		font-weight: 700;
		font-size: 14.5px;
		color: var(--k-text-1);
	}
	.upnext-sub {
		font-size: 12px;
		color: var(--k-text-fainter);
		margin-top: 3px;
	}
	.comments {
		width: 100%;
		max-width: 720px;
		padding: 12px 20px 120px;
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	@media (max-width: 640px) {
		.chapter-header h1 {
			font-size: 24px;
		}
	}
</style>
