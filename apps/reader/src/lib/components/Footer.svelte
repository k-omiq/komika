<script lang="ts">
	let { links = false }: { links?: boolean } = $props();
</script>

<footer class:with-links={links}>
	<span>komiq</span>
	{#if links}
		<div class="links">
			<a href="/donate">Donate</a>
			<a href="/support">Support</a>
		</div>
	{/if}
</footer>

<style>
	footer {
		border-top: 1px solid var(--k-border);
		padding: 28px var(--k-gutter);
		text-align: center;
		color: var(--k-text-ghost);
		font-size: 12.5px;
	}
	footer.with-links {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
		flex-wrap: wrap;
		text-align: left;
	}
	.links {
		display: flex;
		gap: 22px;
	}
	.links a {
		color: var(--k-text-dimmer);
		text-decoration: none;
	}
	.links a:hover {
		color: var(--k-text);
	}
</style>
