/**
 * Shared domain types for Komika.
 *
 * Modeled on the Tachiyomi / Suwayomi source API (SManga / SChapter),
 * extended with Komika's social layer (reviews, ratings, comments) and
 * the adaptive-scan / caching metadata that lives on the backend.
 */

/** A comic of any origin — the catalog indexes all of these. */
export type ComicType = 'MANGA' | 'MANHWA' | 'MANHUA' | 'WEBTOON' | 'COMIC';

/** Publication status. Drives the adaptive scanner (see `Series.scan`). */
export type SeriesStatus = 'ONGOING' | 'COMPLETED' | 'HIATUS' | 'CANCELLED' | 'UNKNOWN';

/** Reader layout modes (full Mihon reader parity). */
export type ReadingMode =
	| 'LTR' // paged, left-to-right
	| 'RTL' // paged, right-to-left (manga)
	| 'VERTICAL' // paged, top-to-bottom
	| 'WEBTOON' // continuous vertical, no page gaps
	| 'CONTINUOUS'; // continuous horizontal

export type Id = string;

/** Adaptive-scan configuration, admin-overridable in the "manga DB" console. */
export interface ScanPolicy {
	/** Rolling average interval between chapters, in hours (derived by backend). */
	avgIntervalHours: number;
	/** Admin override; when set, takes precedence over `avgIntervalHours`. */
	overrideIntervalHours: number | null;
	/** How often to re-poll once a series is "overdue", in minutes. */
	pollEveryMinutes: number;
	/** Effective (folded) paused state: forced override, else auto-by-status. */
	paused: boolean;
	/**
	 * Raw admin overrides — let the console distinguish an explicit choice from
	 * the status default even when they coincide. `null` = no override.
	 */
	statusOverride: SeriesStatus | null;
	pausedOverride: boolean | null;
	/** Raw admin poll-interval override (minutes); `null` = no override. */
	pollEveryMinutesOverride: number | null;
	lastScannedAt: string | null;
	nextScanAt: string | null;
}

/** Aggregate health of the adaptive scan scheduler (admin console). */
export interface ScanStatus {
	librarySize: number;
	overdueCount: number;
	lastTickAt: string | null;
	nextDueAt: string | null;
}

/** A user account as shown in the admin user-management console. */
export interface AdminUser {
	id: Id;
	username: string;
	email: string;
	avatarUrl: string | null;
	isAdmin: boolean;
	isBanned: boolean;
	createdAt: string;
}

/**
 * A pending mid-confidence dedup match awaiting manual admin review
 * (CATALOGUE.md §4). The matcher couldn't confidently auto-merge the source
 * series into `candidateWork`, so an admin confirms or rejects it.
 */
export interface MergeCandidate {
	id: Id;
	sourceSeriesId: Id;
	candidateWorkId: Id;
	/** Title of the canonical work the matcher proposes merging into. */
	candidateTitle: string | null;
	/** Current title of the source series' own (provisional) work. */
	sourceTitle: string | null;
	/** Confidence score in [0,1] that produced the review verdict. */
	score: number;
	/** Which signal produced the match (e.g. `title_corroborated`, `fuzzy`). */
	method: string;
	/** Lifecycle: `pending` until an admin resolves it. */
	status: string;
	createdAt: string;
}

/**
 * The result of the Tier-2 `addSourceSeries` dedup add flow: what the matcher
 * decided for the added source series and which canonical work it resolved to.
 */
export interface MatchResult {
	/** `auto_merge` | `review` | `new` | `existing` (already linked). */
	decision: string;
	/** The canonical work the source series is now linked to. */
	workId: Id;
	/** The pre-existing work the matcher proposed (for `auto_merge`/`review`). */
	matchedWorkId: string | null;
	/** Confidence score in [0,1] that produced the decision, if any. */
	score: number | null;
	/** Which signal produced the match, if any. */
	method: string | null;
	/** The source_series row created/linked. */
	sourceSeriesId: string;
}

/**
 * One row of the canonical updates feed (CATALOGUE.md §6): a mirrored MangaDex
 * work with its most recent stored chapter, served from the `chapter` mirror.
 * Openable in the reader via its `workId` (the `w_`-prefixed canonical id) through
 * the `canonicalSeries` path.
 */
export interface CanonicalUpdate {
	workId: Id;
	mangadexId: string;
	title: string | null;
	isNsfw: boolean;
	/** Proxy-ready MangaDex cover thumbnail URL; null until the cover is synced. */
	coverUrl: string | null;
	/** Latest stored chapter number (string — chapters can be "10.5"). */
	latestChapter: string | null;
	latestChapterTitle: string | null;
	/** Publish time of the latest stored chapter (falls back to ingest time). */
	latestAt: string | null;
}

/**
 * Extension coordinates for a source that a native client fetches through an
 * embedded Suwayomi runtime: where to get the APK and which package/version to
 * load. Null on a WorkSource that Komika fetches MangaDex-natively.
 */
export interface SourceExtension {
	/** Suwayomi extension package name (e.g. `eu.kanade.tachiyomi.extension.en.mangadex`). */
	pkgName: string;
	/** Base URL of the extension repository serving the APK. */
	repoUrl: string;
	/** APK filename within the repo; null when not pinned. */
	apkName: string | null;
	/** Extension version code; null when unknown. */
	versionCode: number | null;
	/** Language code the extension targets (e.g. `en`); null when N/A. */
	lang: string | null;
}

/**
 * One source mapping for a canonical work: which source (and, for Suwayomi, which
 * extension) a native client uses to fetch it, plus the source-local coordinates.
 * The canonical dedup model (CATALOGUE.md §2) may map several of these to one work.
 */
export interface WorkSource {
	/** `"mangadex"` (MangaDex-native fetch) or `"suwayomi"` (via an extension). */
	sourceType: string;
	/** Suwayomi source id. */
	sourceId: string;
	/** Manga id/slug identifying the series within the source. */
	sourceKey: string;
	/** Canonical source URL for the series; null when not available. */
	sourceUrl: string | null;
	isNsfw: boolean;
	/** Language code of this mapping (e.g. `en`); null when N/A. */
	lang: string | null;
	/** Extension coordinates; null for MangaDex-native fetch. */
	extension: SourceExtension | null;
}

/** All source mappings resolved for one canonical work (batch grouping). */
export interface WorkSourceGroup {
	workId: Id;
	sources: WorkSource[];
}

/** Aggregate rating summary for a series. */
export interface RatingSummary {
	/** Mean score on a 1–10 scale. */
	average: number;
	count: number;
	/** Histogram: index 0 => score 1, index 9 => score 10. */
	distribution: number[];
}

export interface Series {
	id: Id;
	title: string;
	altTitles: string[];
	author: string | null;
	artist: string | null;
	description: string | null;
	genres: string[];
	type: ComicType;
	status: SeriesStatus;
	/** Cover image — resolved through an ImageProvider, never fetched directly. */
	coverUrl: string;
	/** Opaque backend source identifier (which Suwayomi extension it came from). */
	sourceId: string;
	rating: RatingSummary;
	chapterCount: number;
	/** True if the current user marked it (raises scanning priority). */
	isMarked: boolean;
	/**
	 * NSFW per the canonical model (CATALOGUE.md §2); false until the series is
	 * catalogued. Discovery/search/updates already filter by the viewer's
	 * `showNsfw` preference server-side — this lets the UI badge/flag it too.
	 */
	isNsfw: boolean;
	scan: ScanPolicy;
	createdAt: string;
	updatedAt: string;
}

export interface Chapter {
	id: Id;
	seriesId: Id;
	/** Chapter number as a float (supports 10.5 interludes). */
	number: number;
	title: string | null;
	pageCount: number;
	uploadedAt: string | null;
	scanlator: string | null;
	// per-user reading state
	read: boolean;
	lastPageRead: number;
	bookmarked: boolean;
	isDownloaded: boolean;
}

/**
 * A single page reference. `sourceUrl` is the upstream image URL supplied by the
 * backend; it is NEVER used as an <img src> directly. An ImageProvider turns it
 * into a displayable URL (Worker-proxied on web, Rust-fetched on native).
 */
export interface Page {
	index: number;
	sourceUrl: string;
	width?: number;
	height?: number;
}

// ---- Social layer ------------------------------------------------------------

export interface UserRef {
	id: Id;
	username: string;
	avatarUrl: string | null;
}

/** Series-level review with a 1–10 score. */
export interface Review {
	id: Id;
	seriesId: Id;
	author: UserRef;
	score: number;
	body: string;
	hasSpoiler: boolean;
	createdAt: string;
	updatedAt: string;
}

/** Comment target: a chapter thread or a series-level discussion. */
export type CommentTargetType = 'chapter' | 'series';

/**
 * A comment on a polymorphic target — a per-chapter thread (`targetType: 'chapter'`)
 * or a series-level discussion (`targetType: 'series'`). Distinct from a Review, which
 * is a scored, one-per-user verdict.
 */
export interface Comment {
	id: Id;
	targetType: CommentTargetType;
	targetId: Id;
	author: UserRef;
	body: string;
	hasSpoiler: boolean;
	createdAt: string;
}

// ---- Discovery ---------------------------------------------------------------

export type DiscoveryFeedKind =
	'POPULAR' | 'TRENDING' | 'RECENTLY_UPDATED' | 'RECENTLY_ADDED' | 'GENRE';

export interface DiscoveryFeed {
	kind: DiscoveryFeedKind;
	title: string;
	/** Set when kind === "GENRE". */
	genre?: string;
	items: Series[];
}

export interface Paginated<T> {
	items: T[];
	page: number;
	hasNextPage: boolean;
	total: number | null;
}
