<script lang="ts">
	import '@komika/ui/reset.css';
	import '@komika/ui/tokens.css';
	import favicon from '$lib/assets/favicon.svg';
	import { page } from '$app/state';
	import { auth, initAuth, logout } from '$lib/auth.svelte';
	import { theme, cycleTheme, initTheme } from '$lib/theme.svelte';

	let { children } = $props();

	$effect(() => {
		void initAuth();
	});
	$effect(() => {
		initTheme();
	});

	const onLogin = $derived(page.url.pathname === '/login');
	const themeLabel = $derived(
		theme.pref === 'dark' ? 'Dark' : theme.pref === 'light' ? 'Light' : 'System',
	);
	const nav = [
		{ href: '/', label: 'Catalog' },
		{ href: '/review', label: 'Review' },
		{ href: '/users', label: 'Users' },
	];
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>Komika · manga DB</title>
</svelte:head>

{#if !auth.ready}
	<div class="boot">Loading…</div>
{:else}
	{#if auth.user && !onLogin}
		<header class="admin-header">
			<div class="brand">
				<span class="mark">KOMIKA</span>
				<span class="sub">manga DB</span>
				<nav class="nav">
					{#each nav as item (item.href)}
						<a
							href={item.href}
							class="nav-link"
							aria-current={page.url.pathname === item.href ? 'page' : undefined}>{item.label}</a
						>
					{/each}
				</nav>
			</div>
			<div class="who">
				<button
					class="theme-toggle"
					onclick={cycleTheme}
					title="Theme: {themeLabel} (click to change)"
					aria-label="Theme: {themeLabel}">{themeLabel}</button
				>
				<span class="username">{auth.user.username}</span>
				<span class="admin-badge">Admin</span>
				<button class="signout" onclick={logout}>Sign out</button>
			</div>
		</header>
	{/if}
	<main class:full={!auth.user || onLogin}>
		{@render children()}
	</main>
{/if}

<style>
	.boot {
		min-height: 100vh;
		display: grid;
		place-items: center;
		color: var(--k-text-dim);
		font-family: var(--k-font-sans);
	}
	.admin-header {
		position: sticky;
		top: 0;
		z-index: 20;
		display: flex;
		align-items: center;
		justify-content: space-between;
		height: 60px;
		padding: 0 var(--k-space-6);
		background: var(--k-glass);
		backdrop-filter: blur(14px);
		border-bottom: 1px solid var(--k-border);
	}
	.brand {
		display: flex;
		align-items: baseline;
		gap: 16px;
	}
	.nav {
		display: flex;
		align-items: center;
		gap: 4px;
	}
	.nav-link {
		padding: 6px 12px;
		border-radius: var(--k-radius-pill);
		font-size: 13px;
		font-weight: 600;
		color: var(--k-text-dim);
		text-decoration: none;
		transition: all 0.15s;
	}
	.nav-link:hover {
		color: var(--k-text);
		background: var(--k-hover-fill);
	}
	.nav-link[aria-current='page'] {
		color: var(--k-text-bright);
		background: var(--k-surface-4);
	}
	.theme-toggle {
		height: 32px;
		padding: 0 14px;
		border-radius: var(--k-radius-pill);
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-2);
		font-family: var(--k-font-sans);
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
		transition: all 0.15s;
	}
	.theme-toggle:hover {
		border-color: var(--k-border-strong);
		color: var(--k-text);
	}
	.mark {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 18px;
		letter-spacing: 0.02em;
		color: var(--k-text-bright);
	}
	.sub {
		font-size: 12px;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	.who {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.username {
		font-size: 13px;
		font-weight: 600;
		color: var(--k-text-2);
	}
	.admin-badge {
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--k-accent-teal);
		border: 1px solid rgba(95, 200, 207, 0.4);
		border-radius: var(--k-radius-sm);
		padding: 2px 6px;
	}
	.signout {
		height: 32px;
		padding: 0 14px;
		border-radius: var(--k-radius-pill);
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-2);
		font-family: var(--k-font-sans);
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
		transition: all 0.15s;
	}
	.signout:hover {
		border-color: var(--k-border-strong);
		color: var(--k-text);
	}
	main {
		max-width: var(--k-max-content);
		margin: 0 auto;
		padding: var(--k-space-8) var(--k-space-6);
	}
	main.full {
		max-width: none;
		padding: 0;
	}
</style>
