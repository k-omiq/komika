import type {
	AdminUser,
	CanonicalUpdate,
	Chapter,
	Comment,
	CommentTargetType,
	DiscoveryFeed,
	Id,
	MatchResult,
	MergeCandidate,
	Page,
	Paginated,
	Review,
	ScanStatus,
	Series,
	SeriesStatus,
	UserRef,
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
	search(query: string, page?: number): Promise<Paginated<Series>>;
	series(id: Id): Promise<Series>;
	chapters(seriesId: Id): Promise<Chapter[]>;
	pages(chapterId: Id): Promise<Page[]>;

	// --- library & progress ---
	mark(seriesId: Id, marked: boolean): Promise<Series>;
	library(): Promise<Series[]>;
	setProgress(chapterId: Id, lastPageRead: number, read: boolean): Promise<void>;

	// --- viewer preferences ---
	/** Set the signed-in user's NSFW visibility preference (CATALOGUE.md §2), returning
	 * the new value. Optional: only the unified Komika API implements it. */
	setShowNsfw?(value: boolean): Promise<boolean>;

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

	// --- social ---
	reviews(seriesId: Id, page?: number): Promise<Paginated<Review>>;
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
	banUser?(userId: Id, banned: boolean): Promise<UserRef>;
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
}

export interface Session {
	token: string;
	user: {
		id: Id;
		username: string;
		avatarUrl: string | null;
		isAdmin: boolean;
		/** Whether this user opted into seeing NSFW-flagged works (CATALOGUE.md §2). */
		showNsfw: boolean;
	};
}

export interface RegisterInput {
	username: string;
	email: string;
	password: string;
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
