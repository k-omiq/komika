<script lang="ts">
	import Icon from '$lib/components/Icon.svelte';
	import Footer from '$lib/components/Footer.svelte';
	let { data } = $props();
	const donateTiers = $derived(data.donateTiers);
	const donateAmounts = $derived(data.donateAmounts);
	const donateAllocation = $derived(data.donateAllocation);

	let tier = $state('patron');
	let amount = $state(15);
	let custom = $state('');
	let customActive = $state(false);

	const donateAmt = $derived(customActive && custom ? parseInt(custom, 10) || 0 : amount);
	const donateLabel = $derived(donateAmt > 0 ? '$' + donateAmt : '');
</script>

<section class="hero k-gutter">
	<div class="hero-badge"><Icon name="heart" size={28} fill="currentColor" /></div>
	<h1>Keep YOMU ad-free and paying its creators</h1>
	<p>
		YOMU runs on reader support — no ads, no data resold. Every contribution funds server costs and
		goes directly to the artists and translators behind the series you love.
	</p>
</section>

<section class="goal-wrap k-gutter">
	<div class="goal">
		<div class="goal-top">
			<span class="goal-title">This month's goal</span>
			<span class="goal-num"><strong>$8,240</strong> of $12,000</span>
		</div>
		<div class="goal-bar"><div class="goal-fill"></div></div>
		<div class="goal-note"><span class="dot"></span>68% funded · 1,904 supporters this month</div>
	</div>
</section>

<section class="tiers-wrap k-gutter">
	<div class="tiers-head">
		<h2>Become a member</h2>
		<p>Cancel anytime · switch tiers whenever you like</p>
	</div>
	<div class="tiers">
		{#each donateTiers as t (t.key)}
			{@const on = tier === t.key}
			<div
				class="tier"
				class:on
				style="--accent:{t.accent};border-color:{on ? t.accent : 'var(--k-border-1)'};background:{on
					? '#131211'
					: 'var(--k-surface)'}"
			>
				{#if t.popular}<span class="popular">Most popular</span>{/if}
				<div class="tier-top">
					<span class="tier-name"><span class="tier-dot"></span>{t.name}</span>
					<div class="tier-price">
						<span class="price">{t.price}</span><span class="per">/ month</span>
					</div>
				</div>
				<div class="perks">
					{#each t.perks as p (p)}
						<div class="perk">
							<Icon name="check" size={15} stroke={t.accent} strokeWidth={2.6} />{p}
						</div>
					{/each}
				</div>
				<button
					class="tier-btn"
					style={on
						? 'background:var(--k-primary);border-color:var(--k-primary);color:var(--k-on-primary)'
						: ''}
					onclick={() => (tier = t.key)}
				>
					{on ? 'Selected' : `Choose ${t.name}`}
				</button>
			</div>
		{/each}
	</div>
</section>

<section class="onetime-wrap k-gutter">
	<div class="onetime">
		<div>
			<h3>Prefer a one-time tip?</h3>
			<p>Give once — 100% of one-time gifts go to the creator fund.</p>
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
			<div class="secure">
				<Icon name="card" size={16} />Secure checkout · card, Apple Pay, PayPal
			</div>
			<button class="donate-btn"
				><Icon name="heart" size={16} fill="currentColor" />Donate {donateLabel}</button
			>
		</div>
	</div>
</section>

<section class="alloc-wrap k-gutter">
	<h2 class="alloc-title">Where your money goes</h2>
	<div class="alloc">
		{#each donateAllocation as a (a.label)}
			<div class="alloc-card">
				<span class="alloc-pct" style="color:{a.color}">{a.pct}</span>
				<span class="alloc-label">{a.label}</span>
				<span class="alloc-desc">{a.desc}</span>
			</div>
		{/each}
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
	.goal-wrap,
	.onetime-wrap {
		display: flex;
		justify-content: center;
		padding-top: 32px;
	}
	.goal {
		width: 100%;
		max-width: 720px;
		border: 1px solid var(--k-border-1);
		border-radius: 16px;
		padding: 24px 28px;
		background: var(--k-surface);
		display: flex;
		flex-direction: column;
		gap: 14px;
	}
	.goal-top {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 16px;
		flex-wrap: wrap;
	}
	.goal-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 17px;
		color: var(--k-text);
	}
	.goal-num {
		font-size: 13.5px;
		color: var(--k-text-dimmer);
	}
	.goal-num strong {
		color: var(--k-text);
	}
	.goal-bar {
		height: 10px;
		border-radius: 6px;
		background: var(--k-border-1);
		overflow: hidden;
	}
	.goal-fill {
		height: 100%;
		width: 68%;
		background: linear-gradient(90deg, var(--k-accent), var(--k-hiatus));
		border-radius: 6px;
	}
	.goal-note {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 13px;
		color: var(--k-text-dimmer);
	}
	.dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--k-ongoing);
	}
	.tiers-wrap {
		padding-top: 56px;
	}
	.tiers-head {
		text-align: center;
		margin-bottom: 34px;
	}
	.tiers-head h2 {
		font-size: 26px;
		margin: 0 0 8px;
		color: var(--k-text-bright);
	}
	.tiers-head p {
		font-size: 14.5px;
		color: var(--k-text-dimmer);
		margin: 0;
	}
	.tiers {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
		gap: 22px;
		max-width: 1080px;
		margin: 0 auto;
	}
	.tier {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 20px;
		border: 1px solid;
		border-radius: 16px;
		padding: 28px 26px;
		transition: transform 0.18s;
	}
	.tier:hover {
		transform: translateY(-4px);
	}
	.popular {
		position: absolute;
		top: -11px;
		left: 26px;
		font-size: 10.5px;
		font-weight: 800;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--k-on-primary);
		background: var(--k-hiatus);
		padding: 4px 10px;
		border-radius: 6px;
	}
	.tier-top {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.tier-name {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 18px;
		color: var(--k-text-bright);
	}
	.tier-dot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: var(--accent);
	}
	.tier-price {
		display: flex;
		align-items: baseline;
		gap: 4px;
	}
	.price {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 38px;
		color: var(--k-text-bright);
	}
	.per {
		font-size: 14px;
		color: var(--k-text-dimmer);
	}
	.perks {
		display: flex;
		flex-direction: column;
		gap: 11px;
	}
	.perk {
		display: flex;
		gap: 10px;
		align-items: flex-start;
		font-size: 13.5px;
		color: var(--k-text-3);
		line-height: 1.4;
	}
	.tier-btn {
		margin-top: auto;
		height: 46px;
		border-radius: 9px;
		font-weight: 700;
		font-size: 14px;
		cursor: pointer;
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text);
		transition: all 0.15s;
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
		justify-content: space-between;
		gap: 16px;
		flex-wrap: wrap;
		padding-top: 20px;
		border-top: 1px solid var(--k-border);
	}
	.secure {
		display: flex;
		align-items: center;
		gap: 10px;
		color: var(--k-text-dimmer);
		font-size: 13px;
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
	.alloc-wrap {
		padding-top: 72px;
		padding-bottom: 20px;
	}
	.alloc-title {
		font-size: 22px;
		margin: 0 0 24px;
		color: var(--k-text-bright);
		text-align: center;
	}
	.alloc {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: 20px;
		max-width: 1000px;
		margin: 0 auto;
	}
	.alloc-card {
		border: 1px solid var(--k-border-1);
		border-radius: 14px;
		padding: 24px;
		background: var(--k-surface);
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.alloc-pct {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 30px;
	}
	.alloc-label {
		font-weight: 700;
		font-size: 15px;
		color: var(--k-text);
	}
	.alloc-desc {
		font-size: 13px;
		color: var(--k-text-dimmer);
		line-height: 1.5;
	}
</style>
