<script lang="ts">
	import '@fontsource-variable/bricolage-grotesque';
	import '@fontsource-variable/manrope';
	import '@komika/ui/reset.css';
	import '@komika/ui/tokens.css';
	import '$lib/app.css';
	import favicon from '$lib/assets/favicon.svg';
	import { initAuth } from '$lib/auth.svelte';

	let { children } = $props();

	// Restore + validate any persisted session once, client-side.
	$effect(() => {
		void initAuth();
	});
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>YOMU</title>
</svelte:head>

{@render children()}
