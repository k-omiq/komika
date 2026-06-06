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

	let resolved = $state('');
	let broken = $state(false);

	$effect(() => {
		const source = src;
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
