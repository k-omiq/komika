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

	$effect(() => {
		const source = src;
		// Web sync path: just track `src` changes; no async round-trip, no blob URL to
		// release. (This branch also re-runs the initial resolve on hydration.)
		if (images.resolveCoverSync) {
			resolved = syncResolve(source);
			broken = false;
			return;
		}
		// Native async path: fetch the blob URL and release it on teardown.
		resolved = '';
		broken = false;
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
	<img
		class="cover-img"
		src={resolved}
		{alt}
		{loading}
		decoding="async"
		referrerpolicy="no-referrer"
		style="object-fit:{fit}"
		onerror={() => (broken = true)}
	/>
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
</style>
