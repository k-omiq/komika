<script lang="ts">
	import { images } from '$lib/context';

	let {
		src = '',
		alt = '',
		loading = 'eager',
		fit = 'cover',
	}: {
		src?: string;
		alt?: string;
		/** Native lazy-load hint — pass 'lazy' for offscreen thumbnails (galleries). */
		loading?: 'eager' | 'lazy';
		/** object-fit; 'contain' for a full-cover viewer, 'cover' (default) to fill. */
		fit?: 'cover' | 'contain';
	} = $props();

	// Web: the provider resolves covers synchronously (a pure URL rewrite), so resolve
	// eagerly — including during SSR — so the <img src> is in the server HTML and the
	// cover counts toward LCP instead of appearing only after a browser-only effect.
	// Native (Tauri): resolveCoverSync is absent (covers are async blob URLs), so we
	// fall back to the async effect below.
	const syncResolve = (s: string): string =>
		s && images.resolveCoverSync ? images.resolveCoverSync(s) : '';

	let resolved = $state(syncResolve(src));
	let broken = $state(false);

	// A cover failure is usually TRANSIENT — a dropped request on a flaky connection,
	// a cold proxy, a rate-limited upstream. `broken` was only ever cleared when `src`
	// changed, so one such blip left a grey placeholder in that slot for the rest of
	// the page's life, even after the network recovered. Retry a bounded number of
	// times with a backoff (the reader does the same for pages via `retryImage`);
	// `attempt` is part of the <img> key so a retry remounts a fresh element rather
	// than relying on the browser to re-request an identical src.
	const MAX_RETRIES = 2;
	let attempt = $state(0);
	let retries = 0;
	let retryTimer: ReturnType<typeof setTimeout> | undefined;

	// There is deliberately NO "a fast failure means the origin refused us, so don't
	// retry" discriminator here. It was tried and removed, because its premises are
	// false against this stack:
	//
	//  • Retrying the saturation case is CORRECT, not wasteful. When the cover pool is
	//    full the API answers `503 + Retry-After: 2 + no-store`
	//    (`cover_warming_response` in apps/server/src/main.rs) and materializes the
	//    cover in the background, so the bytes usually exist within a second or two —
	//    a bounded, jittered retry is exactly what turns that into a cover appearing
	//    instead of a grey block that sits there until the next navigation. The 503 is
	//    also cheap to serve: it is returned by a `try_acquire` that fails instantly,
	//    never by a queued upstream fetch.
	//  • It measured the wrong interval. The clock could only be started at mount, but
	//    every cover in a grid is `loading="lazy"` (MangaCard's default), so the
	//    request begins whenever the row scrolls into view — seconds or minutes later.
	//    "Elapsed" was therefore time-since-mount, and the check was inert in exactly
	//    the grid it was aimed at.
	//  • The failures that DO land under a second are `ERR_INTERNET_DISCONNECTED`,
	//    `ERR_NAME_NOT_RESOLVED` and `ERR_NETWORK_CHANGED` — offline, DNS, and the
	//    Wi-Fi/cellular handoff. That is the transient flaky-mobile case retries exist
	//    for, and it was the one case being suppressed.
	//
	// The real anti-stampede lever is the backoff below, which is kept.
	//
	// Base backoff, plus jitter. Without jitter every failing cover in a grid retries in
	// the same animation frame, so one bad moment becomes a synchronised second wave of
	// ~30 requests. Jitter spreads that wave over the whole window.
	const RETRY_BASE_MS = 1200;
	const RETRY_JITTER_MS = 900;

	function onError(): void {
		if (retries >= MAX_RETRIES) {
			broken = true;
			return;
		}
		retries += 1;
		broken = true; // drop the failed element, then remount a fresh one
		clearTimeout(retryTimer);
		const delay = retries * RETRY_BASE_MS + Math.random() * RETRY_JITTER_MS;
		retryTimer = setTimeout(() => {
			broken = false;
			attempt += 1;
		}, delay);
	}

	/** Clear retry bookkeeping — a new `src` gets a full retry budget of its own. */
	function resetRetries(): void {
		clearTimeout(retryTimer);
		retryTimer = undefined;
		retries = 0;
		attempt = 0;
		broken = false;
	}

	$effect(() => () => clearTimeout(retryTimer));

	$effect(() => {
		const source = src;
		// Web sync path: just track `src` changes; no async round-trip, no blob URL to
		// release. (This branch also re-runs the initial resolve on hydration.)
		if (images.resolveCoverSync) {
			resolved = syncResolve(source);
			resetRetries();
			return;
		}
		// Native async path: fetch the blob URL and release it on teardown.
		resolved = '';
		resetRetries();
		if (!source) return;
		let alive = true;
		images
			.resolveCover(source)
			.then((u) => {
				if (alive) resolved = u;
			})
			.catch(() => {
				if (alive) broken = true;
			});
		return () => {
			alive = false;
			if (resolved && images.release) images.release(resolved);
		};
	});
</script>

{#if resolved && !broken}
	{#key attempt}
		<img
			class="cover-img"
			src={resolved}
			{alt}
			{loading}
			decoding="async"
			referrerpolicy="no-referrer"
			style="object-fit:{fit}"
			onerror={onError}
		/>
	{/key}
{:else}
	<div class="cover-ph k-cover"></div>
{/if}

<style>
	.cover-img,
	.cover-ph {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}
	/* The same cover tone the `.cover-ph` fallback uses, UNDER the image. Two states
	   otherwise render as a see-through hole rather than an unloaded cover: the moment
	   before the bytes arrive, and the API's "warming" answer on cover-pool saturation —
	   a 200 carrying a 1x1 fully-transparent WebP, which loads fine so `onerror` never
	   fires and there is nothing to fall back to. The server picked transparent so "the
	   card's own CSS placeholder shows through", but MangaCard's `.cover` sets no
	   background of its own, so what showed through was the page. Costs one paint and
	   is covered completely by any real cover. */
	.cover-img {
		background: var(--k-cover);
	}
</style>
