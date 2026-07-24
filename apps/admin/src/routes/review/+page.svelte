<script lang="ts">
	import type { MergeCandidate } from '@komika/types';
	import { auth } from '$lib/auth.svelte';
	import {
		consolidateDuplicates,
		loadMergeQueue,
		MERGE_QUEUE_PAGE_SIZE,
		resolveMergeCandidate,
	} from '$lib/data';

	// ---- server-side paging ----------------------------------------------------
	// `mergeQueue` is paginated on the server (page/limit, `total` = the pending
	// count). The backlog is ~10k rows, so only the current page is ever held here.
	let queue = $state<MergeCandidate[]>([]);
	let page = $state(1);
	let hasNext = $state(false);
	let total = $state<number | null>(null);
	let loading = $state(false);
	let loadError = $state<string | null>(null);
	let actionError = $state<string | null>(null);
	let busy = $state<string | null>(null); // candidate id currently being resolved
	/**
	 * Rows resolved since this page was fetched — the OFFSET DRIFT.
	 *
	 * `mergeQueue` is offset-paged (`OFFSET (page-1)*limit`, `ORDER BY score DESC`) over
	 * a list that resolving a row REMOVES from. Resolve k rows on page p and every row
	 * after them shifts k slots earlier, so k rows that were the head of page p+1 fall
	 * back into page p's slot — and asking the server for p+1 steps straight over them.
	 * With a ~10k backlog those candidates are then never seen again.
	 *
	 * The page/limit API can't express "offset 50p − k", so Next re-reads the CURRENT
	 * page while this is non-zero: same 50-slot window, now holding the rows that
	 * shifted in. It converges (the rows we resolved are gone from it) and it never
	 * skips. A full drain is the special case where the shift is exactly one page, and
	 * {@link settleAfterResolve} already re-reads the same page number for it.
	 */
	let resolvedOnPage = $state(0);

	// ---- bulk consolidation (destructive: merge_works DELETEs the folded work) ----
	// One batch of clusters per server call. The run is BOUNDED (never "loop until the
	// server says zero"): an explicit confirm gates the first batch, MAX_BATCHES caps a
	// single run, and Stop halts it between batches. Matching is by normalized alias —
	// which includes alternative titles, not just the primary one — so a run can fold
	// genuinely distinct entries; it stays attended by design.
	const BATCH_SIZE = 200;
	const MAX_BATCHES = 5;

	let consolidating = $state(false);
	let stopRequested = $state(false);
	let consolidateMsg = $state<string | null>(null);
	let batchesDone = $state(0);
	let mergedTotal = $state(0);

	function consolidateConfirmText(): string {
		return (
			`Consolidate duplicate works?\n\n` +
			`This merges works whose NORMALIZED TITLE OR ALTERNATIVE TITLE collides — not ` +
			`only exact primary-title matches. Each merge PERMANENTLY DELETES the folded ` +
			`work; there is no undo.\n\n` +
			`This run processes at most ${MAX_BATCHES} batches of ${BATCH_SIZE} clusters ` +
			`(stop it any time). Review the result before running it again.`
		);
	}

	async function consolidate(): Promise<void> {
		if (consolidating) return;
		if (!confirm(consolidateConfirmText())) return;
		consolidating = true;
		stopRequested = false;
		batchesDone = 0;
		mergedTotal = 0;
		consolidateMsg = 'Starting…';
		let stopped = false;
		let exhausted = false;
		try {
			for (let i = 0; i < MAX_BATCHES; i++) {
				if (stopRequested) {
					stopped = true;
					break;
				}
				const n = await consolidateDuplicates(BATCH_SIZE);
				batchesDone += 1;
				mergedTotal += n;
				consolidateMsg = `Batch ${batchesDone}/${MAX_BATCHES} · merged ${mergedTotal} work${mergedTotal === 1 ? '' : 's'}…`;
				if (n === 0) {
					exhausted = true;
					break;
				}
			}
			const merged = `merged ${mergedTotal} work${mergedTotal === 1 ? '' : 's'} in ${batchesDone} batch${batchesDone === 1 ? '' : 'es'}`;
			consolidateMsg = stopped
				? `Stopped — ${merged}.`
				: exhausted
					? mergedTotal === 0
						? 'No duplicate clusters left to merge.'
						: `Done — ${merged}; nothing left to merge.`
					: `Batch limit reached — ${merged}. Run again to continue.`;
		} catch (err) {
			const why = err instanceof Error ? err.message : 'Consolidation failed.';
			consolidateMsg = `${why} (merged ${mergedTotal} work${mergedTotal === 1 ? '' : 's'} before failing.)`;
		} finally {
			consolidating = false;
			stopRequested = false;
		}
		// Consolidation may have folded works referenced by pending review rows —
		// pull the authoritative queue so stale/merged candidates drop out. Back to
		// page 1: rows leaving anywhere in the backlog shift every page after them.
		if (batchesDone > 0) await refresh(1);
	}

	// Auth redirect is centralized in +layout.svelte.

	$effect(() => {
		if (!auth.user) return;
		void refresh(1);
	});

	async function refresh(p = page): Promise<void> {
		loading = true;
		loadError = null;
		try {
			let res = await loadMergeQueue(p);
			// Landed past the end of a backlog that shrank under us (another admin, or a
			// consolidation run). The server echoes the requested page rather than
			// clamping, and the pager isn't rendered for an empty table — so without this
			// the admin is stranded on "the queue is clear" beside a non-zero count, and
			// Refresh just re-reads the same empty page. Jump to the real last page; one
			// extra round-trip, bounded (the second read is not re-clamped).
			if (res.items.length === 0 && res.page > 1 && (res.total ?? 0) > 0) {
				const last = Math.max(1, Math.ceil((res.total ?? 1) / MERGE_QUEUE_PAGE_SIZE));
				if (last < res.page) res = await loadMergeQueue(last);
			}
			queue = res.items;
			page = res.page;
			hasNext = res.hasNextPage;
			total = res.total ?? null;
			resolvedOnPage = 0;
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'Failed to load the review queue.';
			queue = [];
			hasNext = false;
		} finally {
			loading = false;
		}
	}

	/**
	 * Step forward without stepping OVER anything — see {@link resolvedOnPage}. Once
	 * this page has drifted, `page + 1` is the wrong window and `page` is the right one.
	 */
	function nextPage(): void {
		void refresh(resolvedOnPage > 0 ? page : page + 1);
	}

	/** 1-based index of the first row on this page, within the whole backlog. */
	const rangeStart = $derived(queue.length === 0 ? 0 : (page - 1) * MERGE_QUEUE_PAGE_SIZE + 1);
	const rangeEnd = $derived((page - 1) * MERGE_QUEUE_PAGE_SIZE + queue.length);

	/**
	 * Rows are removed optimistically as they resolve, so a page can drain while the
	 * backlog is still large. Re-pull rather than leave an empty table that reads as
	 * "queue clear": the SAME page number is the right window, because a full drain
	 * shifts the backlog by exactly one page and the server backfills this slot from
	 * what used to be the next one. Resolving the last row on the LAST page reads as
	 * empty, and `refresh()`'s past-the-end clamp walks it back.
	 */
	async function settleAfterResolve(): Promise<void> {
		if (queue.length === 0 && total != null && total > 0) await refresh(page);
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
			// The resolver has no "already resolved" SUCCESS value — every exit path is
			// either an error or a true. So a concurrent resolve arrives as a throw, and
			// the stale-row cleanup has to live in the catch below.
			await resolveMergeCandidate(c.id, accept);
			queue = queue.filter((x) => x.id !== c.id);
			resolvedOnPage += 1;
			if (total != null && total > 0) total -= 1;
			await settleAfterResolve();
		} catch (err) {
			const why = err instanceof Error ? err.message : 'Action failed.';
			// "already resolved" / "no such candidate" => the row is a phantom another
			// admin (or a consolidation run) already closed. Drop it and re-pull the
			// authoritative queue instead of leaving it there to be clicked again.
			const stale = /already resolved|no such merge candidate|not found/i.test(why);
			actionError = stale ? `${why} Refreshing the queue…` : why;
			if (stale) {
				await refresh();
				await settleAfterResolve();
			}
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
				{total ?? queue.length}{total == null && hasNext ? '+' : ''} pending match{(total ??
					queue.length) === 1
					? ''
					: 'es'} · confirm a merge into the canonical work, or keep the series as a distinct entry
			</p>
		</div>
		<div class="head-actions">
			{#if consolidateMsg}<span class="cmsg">{consolidateMsg}</span>{/if}
			{#if consolidating}
				<button class="pg stop" disabled={stopRequested} onclick={() => (stopRequested = true)}>
					{stopRequested ? 'Stopping after this batch…' : 'Stop'}
				</button>
			{/if}
			<button
				class="pg"
				disabled={consolidating}
				onclick={() => consolidate()}
				title="Merge works whose normalized title OR alternative title collides — destructive, deletes the folded work. Runs at most {MAX_BATCHES} batches of {BATCH_SIZE} clusters."
			>
				{consolidating
					? `Consolidating… (batch ${batchesDone + 1}/${MAX_BATCHES})`
					: 'Consolidate duplicates…'}
			</button>
			<button class="pg" disabled={loading || consolidating} onclick={() => refresh()}
				>Refresh</button
			>
		</div>
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
		{#if page > 1 || hasNext}
			<div class="pager">
				<button class="pg" disabled={page <= 1 || loading} onclick={() => refresh(page - 1)}>
					Prev
				</button>
				<span class="page-num">
					Page {page} · showing {rangeStart}–{rangeEnd}{total != null ? ` of ${total}` : ''}
					{#if resolvedOnPage > 0}
						· <span
							class="drift"
							title="Resolving a row removes it from the server's ordered backlog, so the rows behind it shift into this page's slot. Next re-reads this page instead of advancing, otherwise those candidates would be skipped."
							>{resolvedOnPage} resolved here — Next re-reads this page</span
						>
					{/if}
				</span>
				<button class="pg" disabled={!hasNext || loading} onclick={nextPage}> Next </button>
			</div>
		{/if}
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
	.head-actions {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.cmsg {
		font-size: 12.5px;
		color: var(--k-text-dim);
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
	.pg.stop {
		border-color: rgba(224, 131, 105, 0.5);
		color: var(--k-accent);
	}
	.pager {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 16px;
	}
	.page-num {
		font-size: 13px;
		color: var(--k-text-dim);
	}
	.drift {
		color: var(--k-hiatus);
		border-bottom: 1px dotted var(--k-border-4);
		cursor: help;
	}
</style>
