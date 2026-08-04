<script lang="ts">
	import Icon from '$lib/components/Icon.svelte';
	import { config } from '$lib/config';
	import { EXPENSES, REPORT_TOPICS } from '$lib/data/content';
</script>

<svelte:head>
	<title>Support · komiq</title>
	<meta
		name="description"
		content="Keep komiq running, report a problem with the catalogue, or read how the site works."
	/>
</svelte:head>

<section class="hero k-gutter">
	<h1>Support komiq</h1>
	<!-- The lede has to survive `donateUrl` being unset (self-hosters, and any build with
	     PUBLIC_KOMIKA_DONATE_URL empty): the whole Donate section below disappears with it,
	     so a fixed "two things keep it honest" sentence would be promising a section that
	     is not on the page. -->
	<p class="lede">
		komiq is free, ad-free, and stays that way.
		{#if config.donateUrl}
			Two things keep it honest: money for the machines that serve you pages, and readers who tell
			us when the catalogue is wrong.
		{:else}
			What keeps it honest is readers telling us when the catalogue is wrong.
		{/if}
	</p>
	<div class="jump">
		{#if config.donateUrl}
			<a class="jump-link" href="#donate">Donate</a>
		{/if}
		<a class="jump-link" href="#report">Report an issue</a>
		<a class="jump-link" href="/about">How komiq works</a>
	</div>
</section>

{#if config.donateUrl}
	<section class="block k-gutter" id="donate">
		<div class="block-head">
			<div class="block-icon donate-icon"><Icon name="heart" size={20} /></div>
			<div>
				<h2>Donate</h2>
				<p class="block-sub">
					Entirely voluntary, and it unlocks nothing — there is no paid tier to buy. Every series is
					fully readable without paying, by everyone.
				</p>
			</div>
		</div>

		<p class="where">Where it goes:</p>
		<div class="expenses">
			{#each EXPENSES as e (e.title)}
				<div class="expense">
					<span class="expense-icon"><Icon name={e.icon} size={18} /></span>
					<span class="expense-title">{e.title}</span>
					<span class="expense-desc">{e.desc}</span>
				</div>
			{/each}
		</div>

		<div class="cta">
			<a class="btn-primary" href={config.donateUrl} target="_blank" rel="noopener noreferrer">
				<Icon name="heart" size={17} />
				Donate on Ko-fi
			</a>
			<span class="cta-note">One-off or monthly, whatever you like. No account with us needed.</span
			>
		</div>
	</section>
{/if}

<section class="block k-gutter" id="report">
	<div class="block-head">
		<div class="block-icon report-icon"><Icon name="flag" size={20} /></div>
		<div>
			<h2>Report an issue</h2>
			<p class="block-sub">
				The catalogue is stitched together from many sources, and some mistakes are only visible
				from where you're sitting. These go straight to a queue a human reads.
			</p>
		</div>
	</div>

	<div class="topics">
		{#each REPORT_TOPICS as t (t.kind)}
			<a class="topic" href={`/support/report?kind=${t.kind}`}>
				<span class="topic-title">{t.title}</span>
				<span class="topic-desc">{t.desc}</span>
				<span class="topic-go">Report this <Icon name="arrow-right" size={15} /></span>
			</a>
		{/each}
	</div>

	<!-- No response-time promise and no "we'll get back to you": there is no reply channel
	     at all — reports land in an admin queue and nothing emails the reader. Saying so
	     here is better than a reader waiting for an answer that was never coming. -->
	<p class="fine">
		You don't need an account to file a report, and it makes no difference to how we treat it — if
		you're signed in we just record who sent it. Either way we can't reply, so nothing here asks for
		your email.
	</p>
</section>

<section class="block k-gutter last">
	<div class="block-head">
		<div class="block-icon about-icon"><Icon name="shield" size={20} /></div>
		<div>
			<h2>About komiq</h2>
			<p class="block-sub">
				What komiq actually does, what it keeps, and what it doesn't. Rights holders: the same page
				explains how to reach us.
			</p>
		</div>
	</div>
	<a class="btn-secondary" href="/about">Read about komiq <Icon name="arrow-right" size={16} /></a>
</section>

<style>
	.hero {
		padding-top: 72px;
		padding-bottom: 12px;
		max-width: 820px;
	}
	.hero h1 {
		font-size: 44px;
		line-height: 1.05;
		letter-spacing: -0.03em;
		margin: 0 0 16px;
		color: var(--k-text-bright);
	}
	.lede {
		margin: 0;
		font-size: 17px;
		line-height: 1.62;
		color: var(--k-text-2);
	}
	.jump {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
		margin-top: 26px;
	}
	.jump-link {
		padding: 9px 16px;
		border: 1px solid var(--k-border-3);
		border-radius: var(--k-radius-pill);
		background: var(--k-surface);
		font-size: 13.5px;
		font-weight: 700;
		color: var(--k-text-1);
		text-decoration: none;
	}
	.jump-link:hover {
		border-color: var(--k-border-strong);
		color: var(--k-text-bright);
	}
	.block {
		padding-top: 60px;
		max-width: 980px;
		/* `scroll-margin` so the #donate / #report anchors don't land under the fixed header. */
		scroll-margin-top: calc(var(--k-header-h) + 16px);
	}
	.block.last {
		padding-bottom: 90px;
	}
	.block-head {
		display: flex;
		align-items: flex-start;
		gap: 16px;
		margin-bottom: 28px;
	}
	.block-icon {
		flex: 0 0 auto;
		width: 42px;
		height: 42px;
		border-radius: 11px;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.donate-icon {
		background: rgba(224, 179, 84, 0.14);
		color: var(--k-hiatus);
	}
	.report-icon {
		background: rgba(224, 131, 105, 0.14);
		color: var(--k-accent);
	}
	.about-icon {
		background: rgba(95, 200, 207, 0.14);
		color: var(--k-accent-teal);
	}
	.block h2 {
		font-size: 26px;
		margin: 0 0 8px;
		color: var(--k-text-bright);
	}
	.block-sub {
		margin: 0;
		font-size: 15px;
		line-height: 1.6;
		color: var(--k-text-dim);
		max-width: 66ch;
	}
	.where {
		margin: 0 0 16px;
		font-size: 13px;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	/* 340px minimum, not 240: at 240 the four cards land as 3 + 1 orphan on a desktop
	   width. This keeps them a 2×2 block until the viewport can only take one. */
	.expenses {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(340px, 1fr));
		gap: 16px;
	}
	.expense {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 22px;
		border: 1px solid var(--k-border-1);
		border-radius: var(--k-radius-lg);
		background: var(--k-surface);
	}
	.expense-icon {
		color: var(--k-text-dim);
	}
	.expense-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 16px;
		color: var(--k-text);
	}
	.expense-desc {
		font-size: 13.5px;
		line-height: 1.6;
		color: var(--k-text-dimmer);
	}
	.cta {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 18px;
		margin-top: 26px;
	}
	.btn-primary {
		display: inline-flex;
		align-items: center;
		gap: 9px;
		height: 48px;
		padding: 0 26px;
		border-radius: var(--k-radius);
		background: var(--k-primary);
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 15px;
		text-decoration: none;
	}
	.btn-primary:hover {
		background: var(--k-primary-hover);
	}
	.cta-note {
		font-size: 13.5px;
		color: var(--k-text-dimmer);
	}
	.topics {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
		gap: 16px;
	}
	.topic {
		display: flex;
		flex-direction: column;
		gap: 9px;
		padding: 22px;
		border: 1px solid var(--k-border-1);
		border-radius: var(--k-radius-lg);
		background: var(--k-surface);
		text-decoration: none;
		transition:
			border-color 0.15s,
			transform 0.15s;
	}
	.topic:hover {
		border-color: var(--k-border-strong);
		transform: translateY(-2px);
	}
	.topic-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 16.5px;
		color: var(--k-text-bright);
	}
	.topic-desc {
		font-size: 13.5px;
		line-height: 1.6;
		color: var(--k-text-dimmer);
	}
	.topic-go {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		margin-top: 4px;
		font-size: 13px;
		font-weight: 700;
		color: var(--k-accent);
	}
	.fine {
		margin: 22px 0 0;
		font-size: 13.5px;
		line-height: 1.6;
		color: var(--k-text-faint);
		max-width: 70ch;
	}
	.btn-secondary {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		height: 46px;
		padding: 0 22px;
		border: 1px solid var(--k-border-3);
		border-radius: var(--k-radius);
		background: var(--k-surface);
		font-weight: 700;
		font-size: 14.5px;
		color: var(--k-text);
		text-decoration: none;
	}
	.btn-secondary:hover {
		border-color: var(--k-border-strong);
		color: var(--k-text-bright);
	}
	@media (max-width: 640px) {
		.hero {
			padding-top: 44px;
		}
		.hero h1 {
			font-size: 32px;
		}
		.lede {
			font-size: 15.5px;
		}
		.block {
			padding-top: 46px;
		}
		.block h2 {
			font-size: 22px;
		}
		.block-head {
			gap: 12px;
			margin-bottom: 22px;
		}
		.expenses,
		.topics {
			grid-template-columns: 1fr;
		}
	}
</style>
