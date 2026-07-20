/**
 * Admin data layer — catalog listing + override writes, straight through the
 * unified backend (no mock fallback: the console requires the real API).
 */
import type {
	AdminUser,
	CoverIssue,
	BulkAddResult,
	CanonicalUpdate,
	ExtensionInfo,
	Id,
	MatchResult,
	MergeCandidate,
	MergeWorksResult,
	Paginated,
	Series,
	SeriesSourceGroup,
	SeriesStatus,
	SourceBrowsePage,
	SourceBrowseType,
	SourceIngestJob,
	SourceInfo,
	WorkSource,
} from '@komika/types';
import type {
	AdminChapter,
	ChapterOverrideInput,
	SeriesAdminInput,
	SeriesAdminMeta,
	SeriesMetadataInput,
} from '@komika/api';
import { backend } from './context';

/**
 * The catalog the console manages, ONE server page at a time. A text query hits the
 * live source index; an empty query serves the whole persisted catalogue — both
 * paginated SERVER-SIDE by {@link backend.search} (page/pageSize), so the console
 * never pulls the unbounded full library into memory. Returns the paginated envelope
 * (`items` + `page` + `hasNextPage` + `total`) so the caller can drive its pager off
 * the server's own paging metadata.
 */
export async function loadCatalog(query: string, page = 1): Promise<Paginated<Series>> {
	return backend.search(query.trim(), page);
}

/** Persist per-series overrides (whole-state) and return the recomputed series. */
export async function saveSeriesAdmin(input: SeriesAdminInput): Promise<Series> {
	if (!backend.updateSeriesAdmin) throw new Error('Admin API is unavailable on this backend.');
	return backend.updateSeriesAdmin(input);
}

/** Force an immediate re-scan of one series and return its refreshed state. */
export async function triggerScan(seriesId: string): Promise<Series> {
	if (!backend.triggerScan) throw new Error('Manual scan is unavailable on this backend.');
	return backend.triggerScan(seriesId);
}

/** Paginated user list for the user-management console. */
export async function loadUsers(page = 1): Promise<Paginated<AdminUser>> {
	if (!backend.users) throw new Error('User management is unavailable on this backend.');
	return backend.users(page);
}

/** Suspend or restore a user account. */
export async function setUserBanned(userId: string, banned: boolean): Promise<void> {
	if (!backend.banUser) throw new Error('User management is unavailable on this backend.');
	await backend.banUser(userId, banned);
}

/** Grant or revoke a user's admin flag; returns the updated user. */
export async function setUserAdmin(userId: string, isAdmin: boolean): Promise<AdminUser> {
	if (!backend.setUserAdmin) throw new Error('User management is unavailable on this backend.');
	return backend.setUserAdmin(userId, isAdmin);
}

/** Pending dedup matches awaiting manual review (CATALOGUE.md §4). */
export async function loadMergeQueue(): Promise<MergeCandidate[]> {
	if (!backend.mergeQueue) throw new Error('Dedup review is unavailable on this backend.');
	return backend.mergeQueue();
}

/**
 * Resolve a dedup review. `accept` merges the source series into the candidate
 * work; rejecting keeps it as a distinct first-class work. Returns true when the
 * row was closed.
 */
export async function resolveMergeCandidate(id: string, accept: boolean): Promise<boolean> {
	if (!backend.resolveMergeCandidate)
		throw new Error('Dedup review is unavailable on this backend.');
	return backend.resolveMergeCandidate(id, accept);
}

/** Cover "Bugs" panel: works whose cover the crawl couldn't process, paginated. */
export async function loadCoverIssues(page = 1): Promise<Paginated<CoverIssue>> {
	if (!backend.coverIssues) throw new Error('Cover issues are unavailable on this backend.');
	return backend.coverIssues(page);
}

/** Re-attempt cover processing for one work. Returns true if a cover was stored. */
export async function retryCover(workId: string): Promise<boolean> {
	if (!backend.retryCover) throw new Error('Cover retry is unavailable on this backend.');
	return backend.retryCover(workId);
}

/** Replace one work's cover from an uploaded image. Returns the new cover URL. */
export async function uploadCover(workId: string, file: Blob): Promise<string> {
	if (!backend.uploadCover) throw new Error('Cover upload is unavailable on this backend.');
	return backend.uploadCover(workId, file);
}

/**
 * Fold one canonical work into another (admin D1): re-point the source work's
 * source_series mappings + user data to the target, then DELETE the source work.
 * The target survives as canonical. IRREVERSIBLE.
 */
export async function mergeWorks(sourceWorkId: Id, targetWorkId: Id): Promise<MergeWorksResult> {
	if (!backend.mergeWorks) throw new Error('Work merge is unavailable on this backend.');
	return backend.mergeWorks(sourceWorkId, targetWorkId);
}

/**
 * Run the Tier-2 dedup add flow for a Suwayomi manga id: link it to a canonical
 * work (auto-merge / queue for review / create new), idempotently. Returns the
 * matcher's decision.
 */
export async function addSourceSeries(suwayomiMangaId: string): Promise<MatchResult> {
	if (!backend.addSourceSeries)
		throw new Error('Add source series is unavailable on this backend.');
	return backend.addSourceSeries(suwayomiMangaId);
}

/**
 * Recently-updated mirrored MangaDex works + their latest stored chapter, from the
 * canonical `chapter` mirror (CATALOGUE.md §6). A monitoring feed for the mirror —
 * these works are not reader-openable yet.
 */
export async function loadCanonicalUpdates(page = 1): Promise<CanonicalUpdate[]> {
	if (!backend.canonicalUpdates)
		throw new Error('Catalogue updates are unavailable on this backend.');
	return backend.canonicalUpdates(page);
}

// ---- Sources & Extensions console (EXT-2) -----------------------------------

/**
 * Every Keiyoushi/Mihon extension known to the Suwayomi engine (installed or
 * not). First use auto-seeds the curated Keiyoushi store; NSFW extensions are
 * hidden unless the admin opted in via show_nsfw (CATALOGUE.md §2).
 * `refresh: true` re-fetches the store indexes so hasUpdate/versions are fresh.
 */
export async function loadExtensions(refresh = false): Promise<ExtensionInfo[]> {
	if (!backend.extensions) throw new Error('Extension management is unavailable on this backend.');
	return backend.extensions(refresh);
}

/** Register an extension repo by index URL; returns the extension count after refresh. */
export async function addExtensionRepo(indexUrl: string): Promise<number> {
	if (!backend.addExtensionRepo)
		throw new Error('Extension management is unavailable on this backend.');
	return backend.addExtensionRepo(indexUrl);
}

/** Install a store extension onto the Suwayomi engine (NSFW-gated server-side). */
export async function installExtension(pkgName: string): Promise<ExtensionInfo> {
	if (!backend.installExtension)
		throw new Error('Extension management is unavailable on this backend.');
	return backend.installExtension(pkgName);
}

/** Uninstall an extension from the Suwayomi engine. */
export async function uninstallExtension(pkgName: string): Promise<ExtensionInfo> {
	if (!backend.uninstallExtension)
		throw new Error('Extension management is unavailable on this backend.');
	return backend.uninstallExtension(pkgName);
}

/** Update an installed extension to the store's latest version. */
export async function updateExtension(pkgName: string): Promise<ExtensionInfo> {
	if (!backend.updateExtension)
		throw new Error('Extension management is unavailable on this backend.');
	return backend.updateExtension(pkgName);
}

/** The installed Suwayomi sources — the picker feeding {@link browseSource}. */
export async function loadSources(): Promise<SourceInfo[]> {
	if (!backend.sources) throw new Error('Source browsing is unavailable on this backend.');
	return backend.sources();
}

/** Browse/search one source's catalogue (paged) for the bulk-ingest picker. */
export async function browseSource(
	sourceId: Id,
	type: SourceBrowseType,
	page = 1,
	query?: string,
): Promise<SourceBrowsePage> {
	if (!backend.sourceBrowse) throw new Error('Source browsing is unavailable on this backend.');
	return backend.sourceBrowse(sourceId, type, page, query);
}

/**
 * Bulk Tier-2 catalogue ingest: library-track each Suwayomi manga and run the
 * dedup add flow. Per-id failures never abort the batch; at most 100 ids.
 */
export async function bulkAddSourceSeries(suwayomiMangaIds: Id[]): Promise<BulkAddResult> {
	if (!backend.bulkAddSourceSeries)
		throw new Error('Bulk catalogue ingest is unavailable on this backend.');
	return backend.bulkAddSourceSeries(suwayomiMangaIds);
}

/**
 * Catalogue provenance for many Suwayomi series at once: which canonical work
 * each is linked to and every source mapping (with extension coordinates) on
 * that work. One group per id, in input order; max 200 ids per call.
 */
export async function loadSeriesSources(seriesIds: Id[]): Promise<SeriesSourceGroup[]> {
	if (!backend.seriesSourcesBatch)
		throw new Error('Catalogue provenance is unavailable on this backend.');
	return backend.seriesSourcesBatch(seriesIds);
}

/**
 * Pause or unpause one series' scanning (targeted override — leaves the other
 * admin overrides alone). Unpausing triggers an immediate server-side re-scan;
 * returns the recomputed series.
 */
export async function setSeriesPaused(seriesId: Id, paused: boolean): Promise<Series> {
	if (!backend.setSeriesPaused) throw new Error('Pause management is unavailable on this backend.');
	return backend.setSeriesPaused(seriesId, paused);
}

/**
 * The "add all from this source" ingest jobs (S1), newest first. Pass
 * `active: true` for only the currently-running ones — poll it for live
 * progress while a job runs.
 */
export async function loadSourceIngestJobs(active = false): Promise<SourceIngestJob[]> {
	if (!backend.sourceIngestJobs) throw new Error('Source ingest is unavailable on this backend.');
	return backend.sourceIngestJobs(active);
}

/**
 * Start a background ingest that walks a source's whole catalogue through the
 * Tier-2 dedup add flow. Refused while one is already running for the source,
 * and for an NSFW source unless the admin opted in.
 */
export async function startSourceIngest(sourceId: Id): Promise<SourceIngestJob> {
	if (!backend.startSourceIngest) throw new Error('Source ingest is unavailable on this backend.');
	return backend.startSourceIngest(sourceId);
}

/** Request cancellation of a running ingest job; progress so far is preserved. */
export async function cancelSourceIngest(jobId: Id): Promise<SourceIngestJob> {
	if (!backend.cancelSourceIngest) throw new Error('Source ingest is unavailable on this backend.');
	return backend.cancelSourceIngest(jobId);
}

/**
 * Start one ingest job per installed source of an extension (F1). NSFW sources
 * are skipped for an opted-out admin; a source already running returns its
 * existing job instead of erroring. Errors only when no source matches. Returns
 * every started + already-running job.
 */
export async function startExtensionIngest(pkgName: Id): Promise<SourceIngestJob[]> {
	if (!backend.startExtensionIngest)
		throw new Error('Extension ingest is unavailable on this backend.');
	return backend.startExtensionIngest(pkgName);
}

/** Cancel every running ingest job for an extension's sources; returns the cancelled jobs. */
export async function cancelExtensionIngest(pkgName: Id): Promise<SourceIngestJob[]> {
	if (!backend.cancelExtensionIngest)
		throw new Error('Extension ingest is unavailable on this backend.');
	return backend.cancelExtensionIngest(pkgName);
}

/**
 * Subscribe/unsubscribe an extension for background source-sync: while subscribed, the
 * server periodically re-walks the extension's sources to auto-discover newly-added
 * series and keep enrolled series in the library (so they keep updating). Enabling
 * kicks an immediate sync pass server-side. Returns the new subscribed state.
 */
export async function setExtensionSubscription(pkgName: Id, subscribed: boolean): Promise<boolean> {
	if (!backend.setExtensionSubscription)
		throw new Error('Extension sync is unavailable on this backend.');
	return backend.setExtensionSubscription(pkgName, subscribed);
}

/**
 * Maintenance: materialize the whole Suwayomi library into the DB read-cache.
 * Series metadata is written synchronously (the returned count); per-series
 * chapter lists fill in a server-side background task. Admin-gated server-side.
 * Returns how many series were persisted.
 */
export async function persistCatalogue(): Promise<number> {
	if (!backend.persistCatalogue)
		throw new Error('Catalogue persistence is unavailable on this backend.');
	return backend.persistCatalogue();
}

/**
 * Maintenance: materialize every canonical work's cover into the DB
 * (`work_cover_blob`) so the web reader serves covers from `/covers/{id}.webp`
 * instead of the Cloudflare image Worker. Kicks off a polite background crawl and
 * returns how many works are still uncached (queued) at kick-off. Admin-gated.
 */
export async function materializeCatalogueCovers(): Promise<number> {
	if (!backend.materializeCatalogueCovers)
		throw new Error('Cover materialization is unavailable on this backend.');
	return backend.materializeCatalogueCovers();
}

// ---- Series-detail editor (metadata + chapters + rescan) --------------------

/**
 * Load one series for the detail page. A `w_` id routes to the canonical path
 * (`canonicalSeries`, an optional method — guarded with a readable error); a numeric
 * Suwayomi id routes to `series`.
 */
export async function loadSeriesDetail(seriesId: string): Promise<Series> {
	if (seriesId.startsWith('w_')) {
		if (!backend.canonicalSeries)
			throw new Error('Canonical series lookup is unavailable on this backend.');
		return backend.canonicalSeries(seriesId);
	}
	return backend.series(seriesId);
}

/** Raw metadata-override state of a series' canonical work (pinned vs derived). */
export async function loadSeriesAdminMeta(seriesId: string): Promise<SeriesAdminMeta> {
	if (!backend.seriesAdminMeta)
		throw new Error('Series metadata editing is unavailable on this backend.');
	return backend.seriesAdminMeta(seriesId);
}

/** Save metadata overrides (title/description/type/nsfw/tags); returns the series. */
export async function saveSeriesMetadata(input: SeriesMetadataInput): Promise<Series> {
	if (!backend.updateSeriesMetadata)
		throw new Error('Series metadata editing is unavailable on this backend.');
	return backend.updateSeriesMetadata(input);
}

/** The source mappings for one canonical work (per-source rescan + provenance). */
export async function loadWorkSources(workId: string): Promise<WorkSource[]> {
	if (!backend.workSources) throw new Error('Work sources are unavailable on this backend.');
	return backend.workSources(workId);
}

/** A work's aggregated chapters WITH override state (hidden/renamed), unfiltered. */
export async function loadWorkChaptersAdmin(workId: string): Promise<AdminChapter[]> {
	if (!backend.workChaptersAdmin)
		throw new Error('Chapter editing is unavailable on this backend.');
	return backend.workChaptersAdmin(workId);
}

/** Soft-hide (reversible) or rename one chapter of a work. */
export async function setChapterOverride(input: ChapterOverrideInput): Promise<boolean> {
	if (!backend.setChapterOverride)
		throw new Error('Chapter editing is unavailable on this backend.');
	return backend.setChapterOverride(input);
}

/** Force an immediate re-scan of every Suwayomi source of a work; returns count. */
export async function rescanWork(workId: string): Promise<number> {
	if (!backend.rescanWork) throw new Error('Rescan is unavailable on this backend.');
	return backend.rescanWork(workId);
}

export const COMIC_TYPE_OPTIONS = ['MANGA', 'MANHWA', 'MANHUA', 'WEBTOON', 'COMIC'] as const;

export const STATUS_OPTIONS: SeriesStatus[] = [
	'ONGOING',
	'COMPLETED',
	'HIATUS',
	'CANCELLED',
	'UNKNOWN',
];
