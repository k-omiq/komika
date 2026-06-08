<script lang="ts">
	import type { MergeCandidate } from '@komika/types';
	import { auth } from '$lib/auth.svelte';
	import { loadMergeQueue, resolveMergeCandidate } from '$lib/data';

	let queue = $state<MergeCandidate[]>([]);
	let loading = $state(false);
	let loadError = $state<string | null>(null);
	let actionError = $state<string | null>(null);
	let busy = $state<string | null>(null); // candidate id currently being resolved

	// Auth redirect is centralized in +layout.svelte.

	$effect(() => {
		if (!auth.user) return;
		void refresh();
	});

	async function refresh(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			queue = await loadMergeQueue();
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'Failed to load the review queue.';
			queue = [];
		} finally {
			loading = false;
		}
	}

	function fmtDate(iso: string): string {
		const t = Date.parse(iso);
		return Number.isNaN(t) ? '—' : new Date(t).toLocaleDateString();
	}

	function pct(score: number): string {
		return `${Math.round(score * 100)}%`;
	}

	async function resolve(c: MergeCandidate, accept: boolean): Promise<void> {
		if (busy) return;
		if (
			accept &&
			!confirm(
				`Merge “${c.sourceTitle ?? 'this series'}” into “${c.candidateTitle ?? 'the candidate'}”? ` +
					`They'll become one canonical entry.`,
			)
		)
			return;
		busy = c.id;
		actionError = null;
		try {
			const closed = await resolveMergeCandidate(c.id, accept);
			if (closed) {
				queue = queue.filter((x) => x.id !== c.id);
			} else {
				// Another admin already resolved this candidate — don't fake a local
				// removal; surface it and pull the authoritative queue.
				actionError = 'Already resolved by another admin — refreshing…';
				await refresh();
			}
		} catch (err) {
			actionError = err instanceof Error ? err.message : 'Action failed.';
		} finally {
			busy = null;
		}
	}
</script>

<div class="page">
	<div class="page-head">
		<div>
			<h1>Dedup review</h1>
			<p class="lede">
				{queue.length} pending match{queue.length === 1 ? '' : 'es'} · confirm a merge into the canonical
				work, or keep the series as a distinct entry
			</p>
		</div>
		<button class="pg" disabled={loading} onclick={() => refresh()}>Refresh</button>
	</div>

	{#if loadError}
		<div class="notice error">{loadError}</div>
	{:else if loading && queue.length === 0}
		<div class="notice">Loading…</div>
	{:else if queue.length === 0}
		<div class="notice">No matches awaiting review — the queue is clear.</div>
	{:else}
		{#if actionError}<div class="notice error">{actionError}</div>{/if}
		<div class="table" role="table" aria-label="Dedup review queue">
			<div class="row head" role="row">
				<span role="columnheader">Source series</span>
				<span role="columnheader">Candidate work</span>
				<span role="columnheader">Confidence</span>
				<span role="columnheader">Signal</span>
				<span role="columnheader">Queued</span>
				<span role="columnheader" class="actions-col">Decision</span>
			</div>
			{#each queue as c (c.id)}
				<div class="row" role="row">
					<span class="work" role="cell">
						<span class="wtitle">{c.sourceTitle ?? '(untitled)'}</span>
						<span class="wid">{c.sourceSeriesId}</span>
					</span>
					<span class="work" role="cell">
						<span class="wtitle">{c.candidateTitle ?? '(untitled)'}</span>
						<span class="wid">{c.candidateWorkId}</span>
					</span>
					<span role="cell">
						<span class="badge" class:strong={c.score >= 0.75}>{pct(c.score)}</span>
					</span>
					<span class="muted" role="cell">{c.method}</span>
					<span class="muted" role="cell">{fmtDate(c.createdAt)}</span>
					<span class="actions" role="cell">
						<button class="act merge" disabled={busy === c.id} onclick={() => resolve(c, true)}>
							Merge
						</button>
						<button class="act" disabled={busy === c.id} onclick={() => resolve(c, false)}>
							Keep separate
						</button>
					</span>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: var(--k-space-6);
	}
	.page-head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
	}
	h1 {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 30px;
		letter-spacing: -0.02em;
		color: var(--k-text-bright);
	}
	.lede {
		margin-top: 6px;
		font-size: 13.5px;
		color: var(--k-text-dim);
		max-width: 60ch;
	}
	.notice {
		padding: 14px 16px;
		border-radius: var(--k-radius-md);
		background: var(--k-surface-2);
		border: 1px solid var(--k-border);
		color: var(--k-text-dim);
		font-size: 13.5px;
	}
	.notice.error {
		color: #f0808a;
		border-color: rgba(240, 128, 138, 0.4);
	}
	.table {
		border: 1px solid var(--k-border);
		border-radius: var(--k-radius-lg);
		overflow: hidden;
	}
	.row {
		display: grid;
		grid-template-columns: 2fr 2fr 1fr 1.2fr 1fr auto;
		align-items: center;
		gap: var(--k-space-4);
		padding: 12px 16px;
		border-bottom: 1px solid var(--k-border);
	}
	.row:last-child {
		border-bottom: none;
	}
	.row.head {
		background: var(--k-surface-2);
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	.work {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}
	.wtitle {
		font-size: 14px;
		font-weight: 600;
		color: var(--k-text-1);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.wid {
		font-size: 11px;
		color: var(--k-text-faint);
		font-family: var(--k-font-mono, monospace);
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.muted {
		font-size: 13px;
		color: var(--k-text-dim);
	}
	.badge {
		display: inline-block;
		font-size: 11px;
		font-weight: 700;
		padding: 3px 8px;
		border-radius: var(--k-radius-sm);
		background: var(--k-surface-4);
		color: var(--k-text-2);
	}
	.badge.strong {
		background: rgba(95, 200, 207, 0.15);
		color: var(--k-accent-teal);
	}
	.actions-col {
		text-align: right;
	}
	.actions {
		display: flex;
		gap: 8px;
		justify-content: flex-end;
	}
	.act {
		height: 32px;
		padding: 0 12px;
		border-radius: var(--k-radius-md);
		background: var(--k-surface);
		border: 1px solid var(--k-border-4);
		color: var(--k-text-2);
		font-family: var(--k-font-sans);
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
		white-space: nowrap;
		transition: all 0.15s;
	}
	.act:hover:not(:disabled) {
		border-color: var(--k-border-strong);
		color: var(--k-text);
	}
	.act.merge:hover:not(:disabled) {
		border-color: rgba(95, 200, 207, 0.5);
		color: var(--k-accent-teal);
	}
	.act:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.pg {
		height: 36px;
		padding: 0 16px;
		border-radius: var(--k-radius-md);
		background: var(--k-surface);
		border: 1px solid var(--k-border-4);
		color: var(--k-text-2);
		font-family: var(--k-font-sans);
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
	}
	.pg:disabled {
		opacity: 0.4;
		cursor: default;
	}
</style>
