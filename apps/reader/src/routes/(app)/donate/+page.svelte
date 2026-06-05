<script lang="ts">
	import Icon from '$lib/components/Icon.svelte';
	import Footer from '$lib/components/Footer.svelte';
	let { data } = $props();
	const donateAmounts = $derived(data.donateAmounts);

	let amount = $state(15);
	let custom = $state('');
	let customActive = $state(false);

	const donateAmt = $derived(customActive && custom ? parseInt(custom, 10) || 0 : amount);
	const donateLabel = $derived(donateAmt > 0 ? '$' + donateAmt : '');
</script>

<section class="hero k-gutter">
	<div class="hero-badge"><Icon name="heart" size={28} fill="currentColor" /></div>
	<h1>Keep komiq running and ad-free</h1>
	<p>
		komiq is free for everyone and always will be — no ads, no trackers, no data resold. Donations
		are voluntary and help cover the server and hosting costs that keep it online.
	</p>
</section>

<section class="onetime-wrap k-gutter">
	<div class="onetime">
		<div>
			<h3>Make a donation</h3>
			<p>Pick an amount, or enter your own. Every bit helps — there's nothing to unlock, just support.</p>
		</div>
		<div class="amounts">
			{#each donateAmounts as v (v)}
				<button
					class="amount"
					class:on={!customActive && amount === v}
					onclick={() => {
						amount = v;
						customActive = false;
						custom = '';
					}}>${v}</button
				>
			{/each}
			<div class="custom" class:on={customActive}>
				<span class="dollar">$</span>
				<input
					bind:value={custom}
					oninput={() => {
						custom = custom.replace(/[^0-9]/g, '');
						customActive = true;
					}}
					onfocus={() => (customActive = true)}
					placeholder="Other"
					inputmode="numeric"
				/>
			</div>
		</div>
		<div class="checkout">
			<button class="donate-btn"
				><Icon name="heart" size={16} fill="currentColor" />Donate {donateLabel}</button
			>
		</div>
	</div>
</section>

<Footer links />

<style>
	.hero {
		padding-top: 72px;
		padding-bottom: 12px;
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 20px;
	}
	.hero-badge {
		width: 60px;
		height: 60px;
		border-radius: 16px;
		background: rgba(224, 131, 105, 0.14);
		border: 1px solid rgba(224, 131, 105, 0.4);
		display: flex;
		align-items: center;
		justify-content: center;
		color: var(--k-accent);
	}
	.hero h1 {
		font-size: 44px;
		line-height: 1.05;
		letter-spacing: -0.03em;
		margin: 0;
		max-width: 680px;
		color: var(--k-text-bright);
	}
	.hero p {
		max-width: 560px;
		font-size: 16px;
		line-height: 1.65;
		color: var(--k-text-muted);
		margin: 0;
	}
	.onetime-wrap {
		display: flex;
		justify-content: center;
		padding-top: 40px;
		padding-bottom: 32px;
	}
	.onetime {
		width: 100%;
		max-width: 720px;
		border: 1px solid var(--k-border-1);
		border-radius: 16px;
		padding: 30px 32px;
		background: var(--k-surface);
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.onetime h3 {
		font-size: 20px;
		letter-spacing: -0.01em;
		margin: 0 0 6px;
		color: var(--k-text-bright);
	}
	.onetime p {
		font-size: 14px;
		color: var(--k-text-dimmer);
		margin: 0;
	}
	.amounts {
		display: flex;
		gap: 10px;
		flex-wrap: wrap;
	}
	.amount {
		min-width: 74px;
		height: 52px;
		padding: 0 20px;
		border-radius: 10px;
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 17px;
		cursor: pointer;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
		color: var(--k-text-1);
		transition: all 0.15s;
	}
	.amount.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.custom {
		flex: 1;
		min-width: 120px;
		display: flex;
		align-items: center;
		gap: 6px;
		height: 52px;
		padding: 0 16px;
		border-radius: 10px;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-3);
		transition: border-color 0.15s;
	}
	.custom.on {
		border-color: var(--k-border-strong);
	}
	.dollar {
		font-size: 17px;
		color: var(--k-text-dimmer);
		font-weight: 700;
	}
	.custom input {
		flex: 1;
		min-width: 0;
		background: transparent;
		border: none;
		outline: none;
		color: var(--k-text);
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 17px;
	}
	.checkout {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 16px;
		flex-wrap: wrap;
		padding-top: 20px;
		border-top: 1px solid var(--k-border);
	}
	.donate-btn {
		height: 50px;
		padding: 0 30px;
		border: none;
		border-radius: 9px;
		background: var(--k-accent);
		color: var(--k-on-primary);
		font-weight: 800;
		font-size: 15px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 9px;
		transition: filter 0.15s;
	}
	.donate-btn:hover {
		filter: brightness(1.06);
	}
</style>
