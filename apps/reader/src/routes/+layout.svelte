<script lang="ts">
	import '@fontsource-variable/bricolage-grotesque';
	import '@fontsource-variable/manrope';
	import '@komika/ui/reset.css';
	import '@komika/ui/tokens.css';
	import '$lib/app.css';
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
	<title>komiq</title>
	<meta name="description" content="Read manga, together. Track your library, follow updates, and discuss chapters." />

	<link rel="icon" href="/favicon.ico" sizes="any" />
	<link rel="icon" type="image/png" sizes="32x32" href="/favicon-32.png" />
	<link rel="icon" type="image/png" sizes="16x16" href="/favicon-16.png" />
	<link rel="apple-touch-icon" href="/apple-touch-icon.png" />
	<link rel="manifest" href="/manifest.webmanifest" />
	<meta name="theme-color" content="#0c0c0d" />
	<meta name="apple-mobile-web-app-title" content="komiq" />

	<meta property="og:type" content="website" />
	<meta property="og:site_name" content="komiq" />
	<meta property="og:title" content="komiq" />
	<meta property="og:description" content="Read manga, together." />
	<meta property="og:image" content="/og-image.png" />
	<meta name="twitter:card" content="summary_large_image" />
	<meta name="twitter:title" content="komiq" />
	<meta name="twitter:description" content="Read manga, together." />
	<meta name="twitter:image" content="/og-image.png" />
</svelte:head>

{@render children()}
