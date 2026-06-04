<script lang="ts">
	import '@fontsource-variable/bricolage-grotesque';
	import '@fontsource-variable/manrope';
	import '@komika/ui/reset.css';
	import '@komika/ui/tokens.css';
	import '$lib/app.css';
	import favicon from '$lib/assets/favicon.svg';
	import { initAuth } from '$lib/auth.svelte';
	import { initTheme } from '$lib/theme.svelte';

	let { children } = $props();

	// Restore + validate any persisted session once, client-side.
	$effect(() => {
		void initAuth();
	});
	// Restore the theme preference + track OS changes.
	$effect(() => {
		initTheme();
	});
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>komiq</title>
</svelte:head>

{@render children()}
