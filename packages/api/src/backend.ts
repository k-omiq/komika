import type {
	AdminUser,
	AggregatedChapter,
	BulkAddResult,
	CanonicalUpdate,
	Chapter,
	Comment,
	CommentTargetType,
	DiscoveryFeed,
	ExtensionInfo,
	FederatedSearchPage,
	GenreFacet,
	Id,
	MatchResult,
	MergeCandidate,
	MergeWorksResult,
	Page,
	Paginated,
	Review,
	ScanStatus,
	Series,
	SeriesSourceGroup,
	SeriesStatus,
	SourceBrowsePage,
	SourceBrowseType,
	SourceIngestJob,
	SourceInfo,
	WorkSource,
	WorkSourceGroup,
} from '@komika/types';

/**
 * The unified Komika backend (Suwayomi/Tachidesk-style server + our services).
 *
 * It owns: identity, the auto-served catalog, chapter/page metadata, library &
 * reading-progress sync, the social layer, and the admin "manga DB". Image bytes
 * are NOT served here — those flow through an ImageProvider.
 *
 * This is the contract the UI codes against. A concrete GraphQL implementation
 * is wired in `graphql-backend.ts`.
 */
export interface Backend {
	// --- auth ---
	session(): Promise<Session | null>;
	login(username: string, password: string): Promise<Session>;
	register(input: RegisterInput): Promise<Session>;
	logout(): Promise<void>;
	/**
	 * Set (or clear) the bearer token used for authenticated requests. Optional:
	 * only backends with real auth (the unified Komika API) implement it; the
	 * Suwayomi adapter has no auth and omits it. Call after login/logout.
	 */
	setToken?(token: string | null): void;

	// --- discovery / catalog (auto-served, no source picking) ---
	discovery(): Promise<DiscoveryFeed[]>;
	/**
	 * The Updates feed: library series with newly-detected chapters, newest-first.
	 * On the unified Komika API this is driven by the adaptive scanner
	 * (`series_scan_state.last_new_chapter_at`); the Suwayomi adapter approximates
	 * it with the source "Latest" endpoint.
	 */
	updates(page?: number): Promise<Paginated<Series>>;
	/**
	 * Catalogue search. An empty query serves the whole persisted catalogue; a text
	 * query hits the live source index. `filters` (genres / rating range) are applied
	 * server-side (genres match ANY, case-insensitive). Both are honoured on the
	 * unified Komika API. (S4)
	 */
	search(query: string, page?: number, filters?: SearchFilters): Promise<Paginated<Series>>;
	/**
	 * Federated multi-extension catalogue search (S3): fans the query out to every
	 * installed source, dedupes to one canonical work per series, and returns each
	 * work with its per-source translator list. User-facing; NSFW-gated by the
	 * viewer's `show_nsfw` posture (same as {@link search}). Optional: only the
	 * unified Komika API implements it.
	 */
	searchAllSources?(query: string, page?: number): Promise<FederatedSearchPage>;
	/** The full genre/tag facet set across the persisted catalogue (S4), most-common
	 *  first, for the search genre filter. Optional: only the unified Komika API. */
	genreFacets?(): Promise<GenreFacet[]>;
	series(id: Id): Promise<Series>;
	chapters(seriesId: Id): Promise<Chapter[]>;
	/** Multi-source aggregated chapters for a canonical work (S2): one entry per
	 *  chapter number across ALL the work's sources, each carrying per-source
	 *  availability so the reader can pick/​fall-back a source. Optional: only the
	 *  unified Komika API implements it. `workId` is the `w_`-prefixed canonical id. */
	aggregatedChapters?(workId: Id): Promise<AggregatedChapter[]>;
	pages(chapterId: Id): Promise<Page[]>;

	// --- library & progress ---
	mark(seriesId: Id, marked: boolean): Promise<Series>;
	library(): Promise<Series[]>;
	setProgress(chapterId: Id, lastPageRead: number, read: boolean): Promise<void>;

	// --- viewer preferences ---
	/** Set the signed-in user's NSFW visibility preference (CATALOGUE.md §2), returning
	 * the new value. Optional: only the unified Komika API implements it. */
	setShowNsfw?(value: boolean): Promise<boolean>;

	// --- profile ---
	/** Update the viewer's editable profile (display name + bio); a blank field
	 * clears it. Returns the refreshed session user. Optional: only the unified
	 * Komika API implements it. */
	updateProfile?(input: UpdateProfileInput): Promise<Session['user']>;
	/** Upload a new avatar (any JPEG/PNG/WebP). The server squares, downscales, and
	 * re-encodes it as budgeted lossless WebP on the VM data volume; returns the new
	 * avatar URL. Optional: only the unified Komika API implements it. */
	uploadAvatar?(file: Blob): Promise<string>;
	/** The signed-in user's recent activity feed (newest first; empty when signed
	 * out). Optional: only the unified Komika API implements it. */
	myActivity?(limit?: number): Promise<Activity[]>;

	// --- canonical catalogue (MangaDex mirror) ---
	/** Recently-updated mirrored MangaDex works + their latest stored chapter, newest
	 * first, NSFW-filtered by the viewer's preference (CATALOGUE.md §6). A data feed;
	 * open one via {@link canonicalSeries} using its `workId`. Optional: only the
	 * unified Komika API implements it. */
	canonicalUpdates?(page?: number): Promise<CanonicalUpdate[]>;
	/** A MangaDex-mirrored canonical `work` as a {@link Series}, for reader browse/read
	 * (CATALOGUE.md §6). `workId` is the `w_`-prefixed canonical id. NSFW-gated by the
	 * viewer's preference. Optional: only the unified Komika API implements it. */
	canonicalSeries?(workId: Id): Promise<Series>;
	/** Chapters of a canonical work from the stored mirror, deduped to one row per
	 * number (English preferred) and ordered ascending. Each `Chapter.id` is the
	 * MangaDex chapter uuid to pass to {@link canonicalPages}. Optional. */
	canonicalChapters?(workId: Id): Promise<Chapter[]>;
	/** Ordered page images for a mirrored MangaDex chapter via MangaDex@Home;
	 * `chapterId` is the MangaDex chapter uuid. URLs are resolved through the Worker
	 * proxy by the ImageProvider (never hotlinked). Optional. */
	canonicalPages?(chapterId: Id): Promise<Page[]>;
	/** Source mappings + extension coordinates for one canonical work, so a native
	 * client (embedded Suwayomi) can fetch it directly. `workId` is the `w_`-prefixed
	 * canonical id. Optional: only the unified Komika API implements it. */
	workSources?(workId: Id): Promise<WorkSource[]>;
	/** {@link workSources} for many works at once, grouped by `workId` — for warming
	 * a native client's source routing over a list. Optional: only the unified
	 * Komika API implements it. */
	workSourcesBatch?(workIds: Id[]): Promise<WorkSourceGroup[]>;

	// --- social ---
	reviews(seriesId: Id, page?: number): Promise<Paginated<Review>>;
	/** The signed-in viewer's own review for a series, regardless of pagination
	 * (null if none / signed out). Optional: only the unified Komika API implements it. */
	myReview?(seriesId: Id): Promise<Review | null>;
	postReview(input: PostReviewInput): Promise<Review>;
	/** Comments on a chapter thread or a series-level discussion (polymorphic target). */
	comments(targetType: CommentTargetType, targetId: Id, page?: number): Promise<Paginated<Comment>>;
	postComment(input: PostCommentInput): Promise<Comment>;

	// --- admin "manga DB" (requires an admin session) ---
	/** Upsert per-series admin overrides (scan cadence, pause, status). Optional:
	 * only the unified Komika API implements it. */
	updateSeriesAdmin?(input: SeriesAdminInput): Promise<Series>;
	/** Aggregate scan-scheduler health (admin console). Optional: only the
	 * unified Komika API implements it. */
	scanStatus?(): Promise<ScanStatus>;
	/** Force an immediate re-scan of one series, bypassing adaptive gating.
	 * Optional: only the unified Komika API implements it. */
	triggerScan?(seriesId: Id): Promise<Series>;

	// --- admin moderation (requires an admin session) ---
	/** Suspend (`banned: true`) or restore a user account. Optional: only the
	 * unified Komika API implements it. */
	banUser?(userId: Id, banned: boolean): Promise<AdminUser>;
	/** Delete a chapter comment. Returns false if it was already gone. Optional:
	 * only the unified Komika API implements it. */
	deleteComment?(commentId: Id): Promise<boolean>;

	// --- admin user management (requires an admin session) ---
	/** Paginated list of user accounts. Optional: only the unified Komika API
	 * implements it. */
	users?(page?: number): Promise<Paginated<AdminUser>>;
	/** Grant or revoke a user's admin flag. Optional: only the unified Komika API
	 * implements it. */
	setUserAdmin?(userId: Id, isAdmin: boolean): Promise<AdminUser>;

	// --- admin dedup review (requires an admin session) ---
	/** Pending mid-confidence dedup matches awaiting manual review
	 * (CATALOGUE.md §4). Optional: only the unified Komika API implements it. */
	mergeQueue?(): Promise<MergeCandidate[]>;
	/** Resolve a pending match: `accept` merges the source series into the
	 * candidate work; rejecting keeps it as a distinct first-class work. Returns
	 * true when the row was closed. Optional: only the unified Komika API
	 * implements it. */
	resolveMergeCandidate?(id: Id, accept: boolean): Promise<boolean>;
	/** Run the Tier-2 dedup add flow for a Suwayomi manga: link it to a canonical
	 * work (auto-merge / queue for review / create new), idempotently. Optional:
	 * only the unified Komika API implements it. */
	addSourceSeries?(suwayomiMangaId: Id): Promise<MatchResult>;
	/** Fold one canonical work into another: re-point the source work's
	 * source_series mappings + user data (library/progress/reviews/comments) +
	 * aliases/external-ids to the target, then DELETE the source work. The target
	 * survives as canonical. Irreversible. Optional: only the unified Komika API
	 * implements it. */
	mergeWorks?(sourceWorkId: Id, targetWorkId: Id): Promise<MergeWorksResult>;

	// --- admin sources & extensions (requires an admin session; EXT-1/EXT-2) ---
	/** Every Keiyoushi/Mihon extension known to the Suwayomi engine, installed or
	 * not. First use seeds the curated Keiyoushi store; NSFW extensions are hidden
	 * unless the admin opted in via show_nsfw. `refresh: true` re-fetches the
	 * store indexes first so hasUpdate/versions are fresh. Optional: only the
	 * unified Komika API implements it. */
	extensions?(refresh?: boolean): Promise<ExtensionInfo[]>;
	/** The installed Suwayomi sources — the picker feeding {@link sourceBrowse}.
	 * NSFW sources are hidden unless the admin opted in. Optional. */
	sources?(): Promise<SourceInfo[]>;
	/** Browse/search one source's catalogue for bulk ingest. Every returned manga
	 * carries the Suwayomi id {@link bulkAddSourceSeries} consumes. Optional. */
	sourceBrowse?(
		sourceId: Id,
		type: SourceBrowseType,
		page?: number,
		query?: string,
	): Promise<SourceBrowsePage>;
	/** Register an extension repo (store) by its index URL and refresh the list;
	 * returns how many extensions are now known. Optional. */
	addExtensionRepo?(indexUrl: string): Promise<number>;
	/** Install a store extension onto the Suwayomi engine (NSFW-gated). Optional. */
	installExtension?(pkgName: string): Promise<ExtensionInfo>;
	/** Uninstall an extension from the Suwayomi engine. Optional. */
	uninstallExtension?(pkgName: string): Promise<ExtensionInfo>;
	/** Update an installed extension to the store's latest version. Optional. */
	updateExtension?(pkgName: string): Promise<ExtensionInfo>;
	/** Bulk Tier-2 catalogue ingest: for each Suwayomi manga id, library-track it
	 * and run the dedup add flow. Per-id failures never abort the batch; at most
	 * 100 ids per call. Optional. */
	bulkAddSourceSeries?(suwayomiMangaIds: Id[]): Promise<BulkAddResult>;
	/** Catalogue provenance for many Suwayomi series at once: which canonical
	 * work each is linked to and every source mapping (with extension
	 * coordinates) on that work. One group per id, in input order; max 200 ids.
	 * Optional. */
	seriesSourcesBatch?(seriesIds: Id[]): Promise<SeriesSourceGroup[]>;
	/** Pause or unpause one series' scanning (targeted paused_override write —
	 * `updateSeriesAdmin` is whole-state). Unpausing triggers an immediate
	 * server-side re-scan; returns the recomputed series. Optional. */
	setSeriesPaused?(seriesId: Id, paused: boolean): Promise<Series>;

	// --- admin background source-ingest jobs (requires an admin session; S1) ---
	/** The "add all from this source" ingest jobs, newest first. Pass
	 * `active: true` for only currently-running ones — poll it for live progress.
	 * Optional: only the unified Komika API implements it. */
	sourceIngestJobs?(active?: boolean): Promise<SourceIngestJob[]>;
	/** Start a background ingest walking a source's catalogue through the Tier-2
	 * dedup add flow. Refused while one is already running for the source, and for
	 * an NSFW source unless the admin opted in. Optional. */
	startSourceIngest?(sourceId: Id): Promise<SourceIngestJob>;
	/** Request cancellation of a running ingest job; the runner stops between
	 * items, preserving progress. Optional. */
	cancelSourceIngest?(jobId: Id): Promise<SourceIngestJob>;
	/** Start one ingest job per installed source of an extension (F1). NSFW
	 * sources are skipped for an opted-out admin, and a source already running is
	 * returned with its existing job rather than erroring. Errors only when no
	 * source matches the package. Returns every started + already-running job.
	 * Optional: only the unified Komika API implements it. */
	startExtensionIngest?(pkgName: Id): Promise<SourceIngestJob[]>;
	/** Cancel every running ingest job for an extension's sources; returns the
	 * cancelled jobs (empty if none were running). Optional. */
	cancelExtensionIngest?(pkgName: Id): Promise<SourceIngestJob[]>;

	// --- admin maintenance (requires an admin session) ---
	/** Materialize the whole Suwayomi library into the DB read-cache: series
	 * metadata is written synchronously and the count returned; per-series chapter
	 * lists fill in a server-side background task. A production maintenance /
	 * pre-warm action, gated server-side by `require_admin`. Returns how many
	 * series were persisted. Optional: only the unified Komika API implements it. */
	persistCatalogue?(): Promise<number>;
}

/** Server-side catalogue-search filters (S4). All optional; omit to not filter. */
export interface SearchFilters {
	/** Match any of these genres (case-insensitive). */
	genres?: string[];
	/** Inclusive rating bounds on the 0–10 scale. */
	minRating?: number;
	maxRating?: number;
}

export interface Session {
	token: string;
	user: {
		id: Id;
		username: string;
		/** Editable display name; falls back to `username` when null. */
		displayName: string | null;
		/** Editable "about me" text. */
		bio: string | null;
		avatarUrl: string | null;
		isAdmin: boolean;
		/** Whether this user opted into seeing NSFW-flagged works (CATALOGUE.md §2). */
		showNsfw: boolean;
		/** Account creation timestamp (ISO 8601) — the profile "joined" date. */
		joinedAt: string;
	};
}

/** One entry in a user's activity feed (see {@link Backend.myActivity}). */
export interface Activity {
	id: Id;
	/** "review" | "comment" | "library_add". */
	kind: string;
	/** "series" | "chapter" — the kind of thing acted on, when known. */
	targetType: string | null;
	/** The series/chapter id acted on; the client resolves a display title. */
	targetId: Id | null;
	createdAt: string;
}

export interface RegisterInput {
	username: string;
	email: string;
	password: string;
}

/** Editable profile fields. A blank/undefined field clears that value. */
export interface UpdateProfileInput {
	displayName?: string | null;
	bio?: string | null;
}

export interface PostReviewInput {
	seriesId: Id;
	score: number;
	body: string;
	hasSpoiler: boolean;
}

export interface PostCommentInput {
	targetType: CommentTargetType;
	targetId: Id;
	body: string;
	hasSpoiler: boolean;
}

/** Admin "manga DB" overrides. Whole-state: null clears an override. */
export interface SeriesAdminInput {
	seriesId: Id;
	overrideIntervalHours?: number | null;
	pollEveryMinutes?: number | null;
	paused?: boolean | null;
	status?: SeriesStatus | null;
}
