<script lang="ts">
	import { images } from '$lib/context';

	let { src = '', alt = '' }: { src?: string; alt?: string } = $props();

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
	<img class="cover-img" src={resolved} {alt} onerror={() => (broken = true)} />
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
	.cover-img {
		object-fit: cover;
	}
</style>
