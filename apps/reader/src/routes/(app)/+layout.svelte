<script lang="ts">
	import Header from '$lib/components/Header.svelte';

	let { children } = $props();
</script>

<a class="k-skip-link" href="#main-content">Skip to content</a>
<Header />
<main id="main-content" tabindex="-1">
	{@render children()}
</main>

<style>
	main {
		min-height: calc(100vh - var(--k-header-h));
	}
</style>
