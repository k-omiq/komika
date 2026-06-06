<script lang="ts">
	import { goto } from '$app/navigation';
	import type { MatchResult, Series, SeriesSourceGroup, SeriesStatus, WorkSource } from '@komika/types';
	import { auth } from '$lib/auth.svelte';
	import {
		loadCatalog,
		loadSeriesSources,
		saveSeriesAdmin,
		setSeriesPaused,
		triggerScan,
		addSourceSeries,
		STATUS_OPTIONS,
	} from '$lib/data';

	let query = $state('');
	let series = $state<Series[]>([]);
	let loading = $state(false);
	let loadError = $state<string | null>(null);

	// ---- source/extension provenance (seriesSourcesBatch) ----
	let provenance = $state<Record<string, SeriesSourceGroup>>({});
	let provenanceError = $state<string | null>(null);

	/** Fetch provenance for the loaded page in one batched call (≤200 ids). */
	async function loadProvenance(list: Series[]): Promise<void> {
		provenanceError = null;
		provenance = {};
		if (list.length === 0) return;
		try {
			const groups = await loadSeriesSources(list.slice(0, 200).map((s) => s.id));
			const map: Record<string, SeriesSourceGroup> = {};
			for (const g of groups) map[String(g.seriesId)] = g;
			provenance = map;
		} catch (err) {
			provenanceError =
				err instanceof Error ? err.message : 'Failed to load source provenance.';
		}
	}

	/** Compact label for one source mapping: extension pkg tail + lang, or the source type. */
	function extLabel(src: WorkSource): string {
		const pkg = src.extension?.pkgName;
		const name = pkg ? (pkg.split('.').pop() ?? pkg) : src.sourceType;
		return src.lang ? `${name} (${src.lang})` : name;
	}

	// ---- targeted pause toggle ----
	let unpausing = $state<string | null>(null);
	let pauseMsg = $state<Record<string, string>>({});

	async function unpause(s: Series): Promise<void> {
		if (unpausing) return;
		unpausing = s.id;
		const { [s.id]: _drop, ...rest } = pauseMsg;
		pauseMsg = rest;
		try {
			const updated = await setSeriesPaused(s.id, false);
			series = series.map((x) => (x.id === updated.id ? updated : x));
		} catch (err) {
			pauseMsg = { ...pauseMsg, [s.id]: err instanceof Error ? err.message : 'Unpause failed.' };
		} finally {
			unpausing = null;
		}
	}

	// Redirect out if we're not an admin.
	$effect(() => {
		if (auth.ready && !auth.user) goto('/login');
	});

	// Initial load once authed.
	$effect(() => {
		if (!auth.user) return;
		void refresh('');
	});

	async function refresh(q: string): Promise<void> {
		loading = true;
		loadError = null;
		try {
			series = await loadCatalog(q);
			void loadProvenance(series);
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'Failed to load catalog.';
			series = [];
			provenance = {};
		} finally {
			loading = false;
		}
	}

	function onSearch(e: SubmitEvent): void {
		e.preventDefault();
		void refresh(query);
	}

	// ---- add to canonical catalogue (Tier-2 dedup add flow) ----
	let addMsg = $state<Record<string, string>>({});
	let addingId = $state<string | null>(null);

	function decisionLabel(r: MatchResult): string {
		switch (r.decision) {
			case 'auto_merge':
				return `auto-merged into ${r.workId}`;
			case 'review':
				return 'queued for review';
			case 'new':
				return 'added as new';
			case 'existing':
				return 'already added';
			default:
				return r.decision;
		}
	}

	async function onAdd(s: Series): Promise<void> {
		addingId = s.id;
		try {
			const r = await addSourceSeries(s.id);
			addMsg = { ...addMsg, [s.id]: decisionLabel(r) };
		} catch (err) {
			addMsg = { ...addMsg, [s.id]: err instanceof Error ? err.message : 'Add failed.' };
		} finally {
			addingId = null;
		}
	}

	// ---- editor ----
	type Scanner = 'auto' | 'paused' | 'running';
	let editing = $state<Series | null>(null);
	let fStatus = $state<'source' | SeriesStatus>('source');
	let fInterval = $state('');
	let fPoll = $state('');
	let fScanner = $state<Scanner>('auto');
	let saving = $state(false);
	let saveError = $state<string | null>(null);
	let scanning = $state(false);
	let scanMsg = $state<string | null>(null);

	function openEditor(s: Series): void {
		editing = s;
		saveError = null;
		scanMsg = null;
		// Decode straight from the raw admin overrides so an explicit choice is
		// never mistaken for the status default (even when they coincide).
		fStatus = s.scan.statusOverride ?? 'source';
		fInterval = s.scan.overrideIntervalHours != null ? String(s.scan.overrideIntervalHours) : '';
		fPoll = s.scan.pollEveryMinutesOverride == null ? '' : String(s.scan.pollEveryMinutesOverride);
		fScanner =
			s.scan.pausedOverride == null ? 'auto' : s.scan.pausedOverride ? 'paused' : 'running';
	}

	function closeEditor(): void {
		editing = null;
	}

	async function save(): Promise<void> {
		if (!editing || saving) return;
		saving = true;
		saveError = null;
		try {
			const interval = fInterval.trim() === '' ? null : Number(fInterval);
			const poll = fPoll.trim() === '' ? null : Number(fPoll);
			if (interval != null && (!Number.isFinite(interval) || interval <= 0))
				throw new Error('Override interval must be a positive number of hours.');
			if (poll != null && (!Number.isInteger(poll) || poll <= 0))
				throw new Error('Poll cadence must be a whole number of minutes greater than zero.');
			const updated = await saveSeriesAdmin({
				seriesId: editing.id,
				overrideIntervalHours: interval,
				pollEveryMinutes: poll,
				paused: fScanner === 'auto' ? null : fScanner === 'paused',
				status: fStatus === 'source' ? null : fStatus,
			});
			series = series.map((s) => (s.id === updated.id ? updated : s));
			editing = null;
		} catch (err) {
			saveError = err instanceof Error ? err.message : 'Failed to save.';
		} finally {
			saving = false;
		}
	}

	async function scanNow(): Promise<void> {
		if (!editing || scanning) return;
		scanning = true;
		scanMsg = null;
		try {
			const updated = await triggerScan(editing.id);
			series = series.map((s) => (s.id === updated.id ? updated : s));
			editing = updated;
			const at = updated.scan.lastScannedAt;
			scanMsg = at ? `Scanned just now (${new Date(at).toLocaleTimeString()}).` : 'Scan complete.';
		} catch (err) {
			scanMsg = err instanceof Error ? err.message : 'Scan failed.';
		} finally {
			scanning = false;
		}
	}

	function cadence(s: Series): string {
		if (s.scan.overrideIntervalHours != null) return `${s.scan.overrideIntervalHours}h · override`;
		if (s.scan.avgIntervalHours > 0) return `${s.scan.avgIntervalHours}h · avg`;
		return 'auto';
	}

	const STATUS_CLASS: Record<SeriesStatus, string> = {
		ONGOING: 'ongoing',
		COMPLETED: 'completed',
		HIATUS: 'hiatus',
		CANCELLED: 'cancelled',
		UNKNOWN: 'unknown',
	};
</script>

<div class="page">
	<div class="page-head">
		<div>
			<h1>Catalog</h1>
			<p class="lede">
				{query.trim() ? 'Search results' : 'Library'} · {series.length} series · manage scan cadence and
				status
			</p>
		</div>
		<form class="search" onsubmit={onSearch}>
			<input placeholder="Search the catalog…" bind:value={query} />
			<button type="submit">Search</button>
			{#if query.trim()}
				<button
					type="button"
					class="clear"
					onclick={() => {
						query = '';
						void refresh('');
					}}>Clear</button
				>
			{/if}
		</form>
	</div>

	{#if loadError}
		<div class="notice error">{loadError}</div>
	{/if}
	{#if provenanceError}
		<div class="notice error">Source provenance unavailable: {provenanceError}</div>
	{/if}

	<div class="table" class:dim={loading}>
		<div class="thead">
			<span class="col-series">Series</span>
			<span class="col-type">Type</span>
			<span class="col-status">Status</span>
			<span class="col-ch">Chapters</span>
			<span class="col-src">Source</span>
			<span class="col-scan">Scan</span>
			<span class="col-actions"></span>
		</div>
		{#if loading && series.length === 0}
			<div class="empty">Loading…</div>
		{:else if series.length === 0}
			<div class="empty">No series. Add some to the library in the reader, or search above.</div>
		{:else}
			{#each series as s (s.id)}
				<div class="row">
					<div class="col-series cell-series">
						{#if s.coverUrl}
							<img
								class="cover"
								src={s.coverUrl}
								alt=""
								loading="lazy"
								referrerpolicy="no-referrer"
							/>
						{:else}
							<div class="cover placeholder"></div>
						{/if}
						<div class="titles">
							<span class="title">{s.title}</span>
							<span class="meta">#{s.id}{s.author ? ` · ${s.author}` : ''}</span>
						</div>
					</div>
					<span class="col-type">{s.type}</span>
					<span class="col-status">
						<span class="badge {STATUS_CLASS[s.status]}">{s.status}</span>
					</span>
					<span class="col-ch">{s.chapterCount}</span>
					<span class="col-src">
						{#if provenance[s.id]}
							{#if provenance[s.id].workId && provenance[s.id].sources.length > 0}
								{#each provenance[s.id].sources as src (`${src.sourceType}:${src.sourceId}:${src.sourceKey}`)}
									<span
										class="src-entry"
										title="{src.extension?.pkgName ?? src.sourceType} · source {src.sourceId}"
									>
										<span class="src-name">{extLabel(src)}</span>
										<span class="src-id">{src.sourceId}</span>
									</span>
								{/each}
							{:else}
								<span class="src-none">not catalogued</span>
							{/if}
						{:else}
							<span class="src-none">—</span>
						{/if}
					</span>
					<span class="col-scan">
						<span class="cadence">{cadence(s)}</span>
						{#if s.scan.paused}
							<span class="paused-badge">Paused</span>
							<button class="unpause" onclick={() => unpause(s)} disabled={unpausing === s.id}>
								{unpausing === s.id ? 'Unpausing…' : 'Unpause'}
							</button>
						{:else}
							<span class="scan-state active">every {s.scan.pollEveryMinutes}m</span>
						{/if}
						{#if pauseMsg[s.id]}<span class="pause-err">{pauseMsg[s.id]}</span>{/if}
					</span>
					<span class="col-actions">
						<button class="edit" onclick={() => openEditor(s)}>Edit</button>
						<button class="add" onclick={() => onAdd(s)} disabled={addingId === s.id}>
							{addingId === s.id ? 'Adding…' : 'Add'}
						</button>
						{#if addMsg[s.id]}<span class="add-msg">{addMsg[s.id]}</span>{/if}
					</span>
				</div>
			{/each}
		{/if}
	</div>
</div>

{#if editing}
	<button class="scrim" aria-label="Close editor" onclick={closeEditor}></button>
	<aside class="drawer">
		<div class="drawer-head">
			<div>
				<div class="drawer-kicker">EDIT · #{editing.id}</div>
				<h2>{editing.title}</h2>
			</div>
			<button class="x" aria-label="Close" onclick={closeEditor}>✕</button>
		</div>

		<div class="field">
			<span class="label">Status flag</span>
			<select bind:value={fStatus}>
				<option value="source">Source default</option>
				{#each STATUS_OPTIONS as opt (opt)}
					<option value={opt}>{opt}</option>
				{/each}
			</select>
			<span class="help">Overrides the status reported by the source.</span>
		</div>

		<div class="field">
			<span class="label">Scanner</span>
			<div class="segmented">
				<button class:on={fScanner === 'auto'} onclick={() => (fScanner = 'auto')}>Auto</button>
				<button class:on={fScanner === 'running'} onclick={() => (fScanner = 'running')}
					>Force run</button
				>
				<button class:on={fScanner === 'paused'} onclick={() => (fScanner = 'paused')}
					>Force pause</button
				>
			</div>
			<span class="help">Auto pauses for completed / hiatus / cancelled series.</span>
		</div>

		<div class="field">
			<span class="label">Scan interval override (hours)</span>
			<input type="text" inputmode="decimal" placeholder="auto (from avg)" bind:value={fInterval} />
			<span class="help">Blank = derive cadence from average time between chapters.</span>
		</div>

		<div class="field">
			<span class="label">Poll cadence when overdue (minutes)</span>
			<input type="text" inputmode="numeric" placeholder="30" bind:value={fPoll} />
		</div>

		<div class="field">
			<span class="label">Manual scan</span>
			<button class="scan-now" onclick={scanNow} disabled={scanning}>
				{scanning ? 'Scanning…' : 'Scan now'}
			</button>
			<span class="help">
				Force an immediate re-scan, ignoring cadence and pause. Last scanned:
				{editing.scan.lastScannedAt
					? new Date(editing.scan.lastScannedAt).toLocaleString()
					: 'never'}.
			</span>
			{#if scanMsg}<span class="help scan-msg">{scanMsg}</span>{/if}
		</div>

		{#if saveError}<div class="notice error">{saveError}</div>{/if}

		<div class="drawer-actions">
			<button class="cancel" onclick={closeEditor}>Cancel</button>
			<button class="save" onclick={save} disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
		</div>
	</aside>
{/if}

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
		gap: var(--k-space-6);
		flex-wrap: wrap;
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
	}
	.search {
		display: flex;
		gap: 8px;
	}
	.search input {
		width: 280px;
		height: 40px;
		padding: 0 14px;
		background: var(--k-surface-2);
		border: 1px solid var(--k-border-2);
		border-radius: var(--k-radius-md);
		color: var(--k-text);
		font-family: var(--k-font-sans);
		font-size: 14px;
	}
	.search input:focus {
		outline: none;
		border-color: var(--k-border-strong);
	}
	.search button {
		height: 40px;
		padding: 0 18px;
		border: none;
		border-radius: var(--k-radius-md);
		background: var(--k-primary);
		color: var(--k-on-primary);
		font-family: var(--k-font-sans);
		font-weight: 700;
		font-size: 13px;
		cursor: pointer;
	}
	.search .clear {
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-2);
	}
	.notice {
		padding: 12px 16px;
		border-radius: var(--k-radius);
		font-size: 13px;
	}
	.notice.error {
		background: rgba(224, 131, 105, 0.1);
		border: 1px solid rgba(224, 131, 105, 0.3);
		color: var(--k-accent);
	}
	.table {
		border: 1px solid var(--k-border);
		border-radius: var(--k-radius-lg);
		overflow: hidden;
		transition: opacity 0.15s;
	}
	.table.dim {
		opacity: 0.6;
	}
	.thead,
	.row {
		display: grid;
		grid-template-columns: minmax(220px, 3fr) 80px 120px 80px minmax(130px, 1.6fr) 150px 80px;
		align-items: center;
		gap: 16px;
		padding: 12px 20px;
	}
	.thead {
		background: var(--k-surface);
		border-bottom: 1px solid var(--k-border);
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--k-text-faint);
	}
	.row {
		border-bottom: 1px solid rgba(255, 255, 255, 0.04);
		font-size: 14px;
		color: var(--k-text-2);
	}
	.row:last-child {
		border-bottom: none;
	}
	.row:hover {
		background: var(--k-hover-fill);
	}
	.cell-series {
		display: flex;
		align-items: center;
		gap: 14px;
		min-width: 0;
	}
	.cover {
		flex: 0 0 auto;
		width: 38px;
		height: 54px;
		object-fit: cover;
		border-radius: 5px;
		background: var(--k-cover);
		border: 1px solid var(--k-border-1);
	}
	.cover.placeholder {
		background-image: var(--k-hatch);
	}
	.titles {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
	}
	.title {
		font-weight: 600;
		color: var(--k-text-1);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.meta {
		font-size: 12px;
		color: var(--k-text-fainter);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.badge {
		display: inline-block;
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.04em;
		padding: 3px 9px;
		border-radius: var(--k-radius-pill);
		border: 1px solid transparent;
	}
	.badge.ongoing {
		color: var(--k-ongoing);
		background: rgba(127, 211, 154, 0.12);
		border-color: rgba(127, 211, 154, 0.35);
	}
	.badge.completed {
		color: var(--k-completed);
		background: rgba(143, 184, 255, 0.12);
		border-color: rgba(143, 184, 255, 0.35);
	}
	.badge.hiatus {
		color: var(--k-hiatus);
		background: rgba(224, 179, 84, 0.12);
		border-color: rgba(224, 179, 84, 0.35);
	}
	.badge.cancelled {
		color: var(--k-accent);
		background: rgba(224, 131, 105, 0.12);
		border-color: rgba(224, 131, 105, 0.35);
	}
	.badge.unknown {
		color: var(--k-text-faint);
		background: var(--k-hover-fill);
		border-color: var(--k-border-3);
	}
	.col-src {
		display: flex;
		flex-direction: column;
		gap: 4px;
		min-width: 0;
	}
	.src-entry {
		display: flex;
		flex-direction: column;
		gap: 1px;
		min-width: 0;
	}
	.src-name {
		font-size: 12.5px;
		font-weight: 600;
		color: var(--k-text-1);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.src-id {
		font-size: 10.5px;
		color: var(--k-text-fainter);
		font-family: var(--k-font-mono, monospace);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.src-none {
		font-size: 12px;
		color: var(--k-text-faint);
	}
	.col-scan {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 3px;
	}
	.cadence {
		font-size: 13px;
		color: var(--k-text-2);
	}
	.scan-state {
		font-size: 11.5px;
		font-weight: 600;
	}
	.scan-state.active {
		color: var(--k-text-faint);
	}
	.paused-badge {
		display: inline-block;
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--k-hiatus);
		background: rgba(224, 179, 84, 0.12);
		border: 1px solid rgba(224, 179, 84, 0.35);
		border-radius: var(--k-radius-sm);
		padding: 2px 6px;
	}
	.unpause {
		height: 24px;
		padding: 0 10px;
		border-radius: var(--k-radius);
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-1);
		font-family: var(--k-font-sans);
		font-weight: 600;
		font-size: 11.5px;
		cursor: pointer;
		transition: all 0.15s;
	}
	.unpause:hover:not(:disabled) {
		border-color: var(--k-border-strong);
	}
	.unpause:disabled {
		opacity: 0.6;
		cursor: default;
	}
	.pause-err {
		font-size: 11px;
		color: #f0808a;
	}
	.col-actions {
		text-align: right;
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 8px;
		flex-wrap: wrap;
	}
	.edit,
	.add {
		height: 32px;
		padding: 0 14px;
		border-radius: var(--k-radius);
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-1);
		font-family: var(--k-font-sans);
		font-weight: 600;
		font-size: 12.5px;
		cursor: pointer;
		transition: all 0.15s;
	}
	.edit:hover,
	.add:hover {
		border-color: var(--k-border-strong);
	}
	.add:disabled {
		opacity: 0.6;
		cursor: default;
	}
	.add-msg {
		font-size: 12px;
		color: var(--k-text-faint);
	}
	.empty {
		padding: 40px 20px;
		text-align: center;
		color: var(--k-text-faint);
		font-size: 14px;
	}

	/* drawer */
	.scrim {
		position: fixed;
		inset: 0;
		z-index: 40;
		border: none;
		background: rgba(6, 6, 7, 0.6);
		cursor: default;
	}
	.drawer {
		position: fixed;
		top: 0;
		right: 0;
		bottom: 0;
		z-index: 41;
		width: 400px;
		max-width: 92vw;
		background: var(--k-surface-2);
		border-left: 1px solid var(--k-border-2);
		box-shadow: -24px 0 60px rgba(0, 0, 0, 0.5);
		padding: var(--k-space-6);
		display: flex;
		flex-direction: column;
		gap: var(--k-space-5);
		overflow-y: auto;
	}
	.drawer-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
	}
	.drawer-kicker {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.08em;
		color: var(--k-text-faint);
		margin-bottom: 6px;
	}
	.drawer-head h2 {
		font-family: var(--k-font-display);
		font-weight: 700;
		font-size: 20px;
		letter-spacing: -0.01em;
		color: var(--k-text-bright);
		line-height: 1.2;
	}
	.x {
		flex: 0 0 auto;
		width: 32px;
		height: 32px;
		border-radius: 50%;
		background: transparent;
		border: 1px solid var(--k-border-3);
		color: var(--k-text-2);
		cursor: pointer;
		font-size: 13px;
	}
	.x:hover {
		border-color: var(--k-border-strong);
		color: var(--k-text);
	}
	.field {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.label {
		font-size: 12px;
		font-weight: 700;
		letter-spacing: 0.02em;
		color: var(--k-text-2);
	}
	.help {
		font-size: 12px;
		color: var(--k-text-fainter);
		line-height: 1.4;
	}
	select,
	.field input {
		height: 42px;
		padding: 0 12px;
		background: var(--k-surface);
		border: 1px solid var(--k-border-2);
		border-radius: var(--k-radius-md);
		color: var(--k-text);
		font-family: var(--k-font-sans);
		font-size: 14px;
	}
	select:focus,
	.field input:focus {
		outline: none;
		border-color: var(--k-border-strong);
	}
	.scan-now {
		height: 42px;
		padding: 0 18px;
		border-radius: var(--k-radius-md);
		background: var(--k-surface);
		border: 1px solid var(--k-border-4);
		color: var(--k-text);
		font-family: var(--k-font-sans);
		font-weight: 600;
		font-size: 13.5px;
		cursor: pointer;
		align-self: flex-start;
	}
	.scan-now:disabled {
		opacity: 0.6;
		cursor: default;
	}
	.scan-msg {
		color: var(--k-text-dim);
	}
	.segmented {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 4px;
		padding: 4px;
		background: var(--k-surface);
		border: 1px solid var(--k-border);
		border-radius: var(--k-radius-md);
	}
	.segmented button {
		height: 34px;
		border: none;
		border-radius: var(--k-radius);
		background: transparent;
		color: var(--k-text-dim);
		font-family: var(--k-font-sans);
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
	}
	.segmented button.on {
		background: var(--k-primary);
		color: var(--k-on-primary);
	}
	.drawer-actions {
		margin-top: auto;
		display: flex;
		gap: 10px;
		justify-content: flex-end;
		padding-top: var(--k-space-4);
	}
	.cancel {
		height: 42px;
		padding: 0 18px;
		border-radius: var(--k-radius-md);
		background: transparent;
		border: 1px solid var(--k-border-4);
		color: var(--k-text-2);
		font-family: var(--k-font-sans);
		font-weight: 600;
		font-size: 13.5px;
		cursor: pointer;
	}
	.save {
		height: 42px;
		padding: 0 24px;
		border: none;
		border-radius: var(--k-radius-md);
		background: var(--k-primary);
		color: var(--k-on-primary);
		font-family: var(--k-font-sans);
		font-weight: 700;
		font-size: 13.5px;
		cursor: pointer;
	}
	.save:disabled {
		opacity: 0.6;
		cursor: default;
	}
</style>
