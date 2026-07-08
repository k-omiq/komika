<script lang="ts">
	import { goto } from '$app/navigation';
	import Icon from './Icon.svelte';
	import { searchFilterSections } from '$lib/data/mock';

	let {
		open = $bindable(false),
		placeholder = 'Search series, authors, genres…',
		advanced = true,
	}: { open?: boolean; placeholder?: string; advanced?: boolean } = $props();

	let advancedOpen = $state(false);
	let q = $state('');
	let inputEl = $state<HTMLInputElement | undefined>();

	$effect(() => {
		if (open && inputEl) setTimeout(() => inputEl?.focus(), 30);
	});

	function close() {
		open = false;
		advancedOpen = false;
	}
	function submit() {
		const term = q.trim();
		close();
		goto(term ? `/browse?q=${encodeURIComponent(term)}` : '/browse');
	}
	function onkey(e: KeyboardEvent) {
		if (e.key === 'Escape') close();
		else if (e.key === 'Enter') submit();
	}
</script>

{#if open}
	<button class="scrim" aria-label="Close search" onclick={close}></button>
	<div class="panel">
		<div class="bar">
			<Icon name="search" size={21} stroke="#87857f" />
			<input bind:this={inputEl} bind:value={q} {placeholder} onkeydown={onkey} />
			{#if advanced}
				<button
					class="adv-btn"
					class:on={advancedOpen}
					aria-label="Advanced search"
					onclick={() => (advancedOpen = !advancedOpen)}
				>
					<Icon name="sliders-h" size={19} />
				</button>
			{:else}
				<button class="esc" onclick={close}>ESC</button>
			{/if}
		</div>

		{#if advanced && advancedOpen}
			<div class="adv-panel">
				<div class="adv-head">
					<span class="adv-title">Advanced search</span>
					<button class="hide" onclick={() => (advancedOpen = false)}>Hide</button>
				</div>
				{#each searchFilterSections as sec (sec.label)}
					<div class="sec">
						<span class="sec-label">{sec.label}</span>
						<div class="chips">
							{#each sec.options as opt (opt)}
								<span class="chip">{opt}</span>
							{/each}
						</div>
					</div>
				{/each}
				<div class="adv-foot">
					<button class="ghost">Reset</button>
					<button class="primary" onclick={submit}>Search</button>
				</div>
			</div>
		{/if}
	</div>
{/if}

<style>
	.scrim {
		position: fixed;
		inset: 0;
		z-index: 40;
		border: none;
		background: rgba(8, 8, 9, 0.55);
		backdrop-filter: blur(4px);
		cursor: default;
	}
	.panel {
		position: fixed;
		z-index: 50;
		top: 84px;
		left: 50%;
		transform: translateX(-50%);
		width: min(700px, calc(100vw - 40px));
		display: flex;
		flex-direction: column;
		gap: 14px;
	}
	.bar {
		display: flex;
		align-items: center;
		gap: 14px;
		height: 62px;
		padding: 0 8px 0 24px;
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-3);
		border-radius: var(--k-radius-pill);
		box-shadow: 0 26px 70px rgba(0, 0, 0, 0.55);
	}
	input {
		flex: 1;
		min-width: 0;
		background: transparent;
		border: none;
		outline: none;
		color: var(--k-text);
		font-size: 16px;
	}
	.adv-btn {
		width: 46px;
		height: 46px;
		flex: 0 0 auto;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		background: var(--k-surface-5);
		border: 1px solid var(--k-border-2);
		color: #a7a59f;
		transition: all 0.15s;
	}
	.adv-btn.on {
		background: var(--k-primary);
		border-color: var(--k-primary);
		color: var(--k-on-primary);
	}
	.esc {
		font-size: 12px;
		font-weight: 700;
		color: var(--k-text-faint);
		border: 1px solid var(--k-border-4);
		border-radius: 6px;
		padding: 4px 8px;
		background: transparent;
		cursor: pointer;
	}
	.adv-panel {
		background: var(--k-surface-3);
		border: 1px solid var(--k-border-3);
		border-radius: var(--k-radius-2xl);
		box-shadow: 0 26px 70px rgba(0, 0, 0, 0.55);
		padding: 24px 26px;
		display: flex;
		flex-direction: column;
		gap: 24px;
	}
	.adv-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.adv-title {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 15px;
		color: var(--k-text);
	}
	.hide {
		background: none;
		border: none;
		font-size: 12.5px;
		color: var(--k-text-dimmer);
		cursor: pointer;
	}
	.sec {
		display: flex;
		flex-direction: column;
		gap: 11px;
	}
	.sec-label {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.09em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 9px;
	}
	.chip {
		font-size: 13px;
		color: var(--k-text-3);
		padding: 7px 14px;
		border-radius: var(--k-radius-pill);
		border: 1px solid var(--k-border-4);
		cursor: pointer;
		white-space: nowrap;
		transition: all 0.15s;
	}
	.chip:hover {
		border-color: rgba(255, 255, 255, 0.36);
		color: var(--k-text);
		background: var(--k-hover-fill);
	}
	.adv-foot {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 12px;
		padding-top: 4px;
		border-top: 1px solid var(--k-border);
	}
	.ghost {
		height: 42px;
		padding: 0 18px;
		background: transparent;
		border: 1px solid var(--k-border-4);
		border-radius: 8px;
		color: var(--k-text-3);
		font-weight: 600;
		font-size: 13.5px;
		cursor: pointer;
	}
	.primary {
		height: 42px;
		padding: 0 24px;
		background: var(--k-primary);
		border: none;
		border-radius: 8px;
		color: var(--k-on-primary);
		font-weight: 700;
		font-size: 13.5px;
		cursor: pointer;
	}
</style>
