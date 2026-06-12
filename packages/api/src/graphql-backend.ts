import type {
	AdminUser,
	AggregatedChapter,
	BulkAddResult,
	CanonicalUpdate,
	Chapter,
	Comment,
	CommentVote,
	Notification,
	CommentTargetType,
	DiscoveryFeed,
	ExtensionInfo,
	FederatedSearchPage,
	GenreFacet,
	Id,
	LibraryStatus,
	MatchResult,
	MergeCandidate,
	MergeWorksResult,
	Page,
	Paginated,
	Review,
	ScanStatus,
	Series,
	SeriesProgress,
	SeriesSourceGroup,
	SourceBrowsePage,
	SourceBrowseType,
	SourceIngestJob,
	SourceInfo,
	WorkSource,
	WorkSourceGroup,
} from '@komika/types';
import type {
	Activity,
	AdminChapter,
	Backend,
	ChapterOverrideInput,
	CommentMediaUpload,
	PostCommentInput,
	PostReviewInput,
	RegisterInput,
	SearchFilters,
	SeriesAdminInput,
	SeriesAdminMeta,
	SeriesMetadataInput,
	Session,
	UpdateProfileInput,
} from './backend.js';
import * as ops from './operations.js';

export interface BackendConfig {
	/** GraphQL endpoint of the unified backend, e.g. https://api.komika.app/graphql */
	endpoint: string;
	/** Bearer token for authenticated requests, if signed in. */
	token?: string | null;
	/** Optional fetch override (SSR / tests). Defaults to global fetch. */
	fetch?: typeof fetch;
}

/**
 * GraphQL-backed implementation of {@link Backend}, targeting Komika's unified
 * API. Operations are domain-shaped (see `operations.ts` + `schema/komika.graphql`),
 * so responses map 1:1 onto `@komika/types` with no translation layer.
 */
export class GraphQLBackend implements Backend {
	constructor(private config: BackendConfig) {}

	setToken(token: string | null): void {
		this.config = { ...this.config, token };
	}

	private async gql<T>(query: string, variables?: Record<string, unknown>): Promise<T> {
		const doFetch = this.config.fetch ?? fetch;
		const res = await doFetch(this.config.endpoint, {
			method: 'POST',
			headers: {
				'content-type': 'application/json',
				...(this.config.token ? { authorization: `Bearer ${this.config.token}` } : {}),
			},
			body: JSON.stringify({ query, variables }),
		});
		if (!res.ok) throw new Error(`Backend error ${res.status}`);
		const json = (await res.json()) as { data?: T; errors?: { message: string }[] };
		if (json.errors?.length) throw new Error(json.errors.map((e) => e.message).join('; '));
		if (json.data == null) throw new Error('Backend returned no data');
		return json.data;
	}

	// --- auth ---
	async session(): Promise<Session | null> {
		const d = await this.gql<{ session: Session | null }>(ops.SESSION);
		return d.session;
	}
	async login(username: string, password: string): Promise<Session> {
		const d = await this.gql<{ login: Session }>(ops.LOGIN, { username, password });
		return d.login;
	}
	async register(input: RegisterInput): Promise<Session> {
		const d = await this.gql<{ register: Session }>(ops.REGISTER, { input });
		return d.register;
	}
	async logout(): Promise<void> {
		await this.gql<{ logout: boolean }>(ops.LOGOUT);
	}

	// --- catalog ---
	async discovery(): Promise<DiscoveryFeed[]> {
		const d = await this.gql<{ discovery: DiscoveryFeed[] }>(ops.DISCOVERY);
		return d.discovery;
	}
	async updates(page = 1): Promise<Paginated<Series>> {
		const d = await this.gql<{ updates: Paginated<Series> }>(ops.UPDATES, { page });
		return d.updates;
	}
	async search(query: string, page = 1, filters?: SearchFilters): Promise<Paginated<Series>> {
		const d = await this.gql<{ search: Paginated<Series> }>(ops.SEARCH, {
			query,
			page,
			genres: filters?.genres && filters.genres.length ? filters.genres : undefined,
			minRating: filters?.minRating,
			maxRating: filters?.maxRating,
		});
		return d.search;
	}
	async searchAllSources(query: string, page = 1): Promise<FederatedSearchPage> {
		const d = await this.gql<{ searchAllSources: FederatedSearchPage }>(ops.SEARCH_ALL_SOURCES, {
			query,
			page,
		});
		return d.searchAllSources;
	}
	async genreFacets(): Promise<GenreFacet[]> {
		const d = await this.gql<{ genreFacets: GenreFacet[] }>(ops.GENRE_FACETS);
		return d.genreFacets;
	}
	async series(id: Id): Promise<Series> {
		const d = await this.gql<{ series: Series }>(ops.SERIES, { id });
		return d.series;
	}
	async chapters(seriesId: Id): Promise<Chapter[]> {
		const d = await this.gql<{ chapters: Chapter[] }>(ops.CHAPTERS, { seriesId });
		return d.chapters;
	}
	async aggregatedChapters(workId: Id): Promise<AggregatedChapter[]> {
		const d = await this.gql<{ aggregatedChapters: AggregatedChapter[] }>(ops.AGGREGATED_CHAPTERS, {
			workId,
		});
		return d.aggregatedChapters;
	}
	async pages(chapterId: Id): Promise<Page[]> {
		const d = await this.gql<{ pages: Page[] }>(ops.PAGES, { chapterId });
		return d.pages;
	}

	// --- library & progress ---
	async mark(seriesId: Id, marked: boolean): Promise<Series> {
		const d = await this.gql<{ mark: Series }>(ops.MARK, { seriesId, marked });
		return d.mark;
	}
	async setLibraryStatus(seriesId: Id, status: LibraryStatus | null): Promise<Series> {
		const d = await this.gql<{ setLibraryStatus: Series }>(ops.SET_LIBRARY_STATUS, {
			seriesId,
			status,
		});
		return d.setLibraryStatus;
	}
	async setFavorite(seriesId: Id, favorite: boolean): Promise<Series> {
		const d = await this.gql<{ setFavorite: Series }>(ops.SET_FAVORITE, { seriesId, favorite });
		return d.setFavorite;
	}
	async library(): Promise<Series[]> {
		const d = await this.gql<{ library: Series[] }>(ops.LIBRARY);
		return d.library;
	}
	async libraryProgress(): Promise<SeriesProgress[]> {
		const d = await this.gql<{ libraryProgress: SeriesProgress[] }>(ops.LIBRARY_PROGRESS);
		return d.libraryProgress;
	}
	async setProgress(chapterId: Id, lastPageRead: number, read: boolean): Promise<void> {
		await this.gql<{ setProgress: boolean }>(ops.SET_PROGRESS, { chapterId, lastPageRead, read });
	}

	async recordView(seriesId: Id): Promise<void> {
		await this.gql<{ recordView: boolean }>(ops.RECORD_VIEW, { seriesId });
	}

	// --- social ---
	async reviews(seriesId: Id, page = 1): Promise<Paginated<Review>> {
		const d = await this.gql<{ reviews: Paginated<Review> }>(ops.REVIEWS, { seriesId, page });
		return d.reviews;
	}
	async myReview(seriesId: Id): Promise<Review | null> {
		const d = await this.gql<{ myReview: Review | null }>(ops.MY_REVIEW, { seriesId });
		return d.myReview;
	}
	async postReview(input: PostReviewInput): Promise<Review> {
		const d = await this.gql<{ postReview: Review }>(ops.POST_REVIEW, { input });
		return d.postReview;
	}
	async comments(
		targetType: CommentTargetType,
		targetId: Id,
		page = 1,
	): Promise<Paginated<Comment>> {
		const d = await this.gql<{ comments: Paginated<Comment> }>(ops.COMMENTS, {
			targetType,
			targetId,
			page,
		});
		return d.comments;
	}
	async uploadCommentMedia(file: Blob): Promise<CommentMediaUpload> {
		const doFetch = this.config.fetch ?? fetch;
		const url = this.config.endpoint.replace(/\/graphql\/?$/, '') + '/comment-media';
		const form = new FormData();
		form.append('image', file);
		const res = await doFetch(url, {
			method: 'POST',
			headers: this.config.token ? { authorization: `Bearer ${this.config.token}` } : {},
			body: form,
		});
		const json = (await res.json().catch(() => null)) as
			(Partial<CommentMediaUpload> & { message?: string }) | null;
		if (!res.ok) throw new Error(json?.message ?? `Upload failed (${res.status})`);
		if (!json?.mediaId || !json.url) throw new Error('Upload returned no media id');
		return {
			mediaId: json.mediaId,
			url: json.url,
			width: json.width ?? 0,
			height: json.height ?? 0,
		};
	}

	async postComment(input: PostCommentInput): Promise<Comment> {
		const d = await this.gql<{ postComment: Comment }>(ops.POST_COMMENT, { input });
		return d.postComment;
	}

	async voteComment(commentId: Id, value: number): Promise<CommentVote> {
		const d = await this.gql<{ voteComment: CommentVote }>(ops.VOTE_COMMENT, { commentId, value });
		return d.voteComment;
	}

	// --- notifications ---
	async notifications(page = 1): Promise<Notification[]> {
		const d = await this.gql<{ notifications: Notification[] }>(ops.NOTIFICATIONS, { page });
		return d.notifications;
	}
	async unreadNotificationCount(): Promise<number> {
		const d = await this.gql<{ unreadNotificationCount: number }>(ops.UNREAD_NOTIFICATION_COUNT, {});
		return d.unreadNotificationCount;
	}
	async markNotificationsRead(ids?: Id[]): Promise<number> {
		const d = await this.gql<{ markNotificationsRead: number }>(ops.MARK_NOTIFICATIONS_READ, {
			ids: ids ?? null,
		});
		return d.markNotificationsRead;
	}

	// --- admin ---
	async updateSeriesAdmin(input: SeriesAdminInput): Promise<Series> {
		const d = await this.gql<{ updateSeriesAdmin: Series }>(ops.UPDATE_SERIES_ADMIN, { input });
		return d.updateSeriesAdmin;
	}

	async scanStatus(): Promise<ScanStatus> {
		const d = await this.gql<{ scanStatus: ScanStatus }>(ops.SCAN_STATUS);
		return d.scanStatus;
	}

	async triggerScan(seriesId: Id): Promise<Series> {
		const d = await this.gql<{ triggerScan: Series }>(ops.TRIGGER_SCAN, { seriesId });
		return d.triggerScan;
	}

	// --- admin series-detail editor ---
	async seriesAdminMeta(seriesId: Id): Promise<SeriesAdminMeta> {
		const d = await this.gql<{ seriesAdminMeta: SeriesAdminMeta }>(ops.SERIES_ADMIN_META, {
			seriesId,
		});
		return d.seriesAdminMeta;
	}

	async updateSeriesMetadata(input: SeriesMetadataInput): Promise<Series> {
		const d = await this.gql<{ updateSeriesMetadata: Series }>(ops.UPDATE_SERIES_METADATA, {
			input,
		});
		return d.updateSeriesMetadata;
	}

	async workChaptersAdmin(workId: Id): Promise<AdminChapter[]> {
		const d = await this.gql<{ workChaptersAdmin: AdminChapter[] }>(ops.WORK_CHAPTERS_ADMIN, {
			workId,
		});
		return d.workChaptersAdmin;
	}

	async setChapterOverride(input: ChapterOverrideInput): Promise<boolean> {
		const d = await this.gql<{ setChapterOverride: boolean }>(ops.SET_CHAPTER_OVERRIDE, { input });
		return d.setChapterOverride;
	}

	async rescanWork(workId: Id): Promise<number> {
		const d = await this.gql<{ rescanWork: number }>(ops.RESCAN_WORK, { workId });
		return d.rescanWork;
	}

	// --- admin moderation ---
	async banUser(userId: Id, banned: boolean): Promise<AdminUser> {
		const d = await this.gql<{ banUser: AdminUser }>(ops.BAN_USER, { userId, banned });
		return d.banUser;
	}

	async deleteComment(commentId: Id): Promise<boolean> {
		const d = await this.gql<{ deleteComment: boolean }>(ops.DELETE_COMMENT, { commentId });
		return d.deleteComment;
	}

	// --- admin user management ---
	async users(page = 1): Promise<Paginated<AdminUser>> {
		const d = await this.gql<{ users: Paginated<AdminUser> }>(ops.USERS, { page });
		return d.users;
	}

	async setUserAdmin(userId: Id, isAdmin: boolean): Promise<AdminUser> {
		const d = await this.gql<{ setUserAdmin: AdminUser }>(ops.SET_USER_ADMIN, { userId, isAdmin });
		return d.setUserAdmin;
	}

	// --- admin dedup review ---
	async mergeQueue(): Promise<MergeCandidate[]> {
		const d = await this.gql<{ mergeQueue: MergeCandidate[] }>(ops.MERGE_QUEUE);
		return d.mergeQueue;
	}

	async resolveMergeCandidate(id: Id, accept: boolean): Promise<boolean> {
		const d = await this.gql<{ resolveMergeCandidate: boolean }>(ops.RESOLVE_MERGE_CANDIDATE, {
			id,
			accept,
		});
		return d.resolveMergeCandidate;
	}

	async addSourceSeries(suwayomiMangaId: Id): Promise<MatchResult> {
		const d = await this.gql<{ addSourceSeries: MatchResult }>(ops.ADD_SOURCE_SERIES, {
			suwayomiMangaId,
		});
		return d.addSourceSeries;
	}

	async mergeWorks(sourceWorkId: Id, targetWorkId: Id): Promise<MergeWorksResult> {
		const d = await this.gql<{ mergeWorks: MergeWorksResult }>(ops.MERGE_WORKS, {
			sourceWorkId,
			targetWorkId,
		});
		return d.mergeWorks;
	}

	// --- admin sources & extensions (EXT-1/EXT-2) ---
	async extensions(refresh = false): Promise<ExtensionInfo[]> {
		const d = await this.gql<{ extensions: ExtensionInfo[] }>(ops.EXTENSIONS, { refresh });
		return d.extensions;
	}

	async sources(): Promise<SourceInfo[]> {
		const d = await this.gql<{ sources: SourceInfo[] }>(ops.SOURCES);
		return d.sources;
	}

	async sourceBrowse(
		sourceId: Id,
		type: SourceBrowseType,
		page = 1,
		query?: string,
	): Promise<SourceBrowsePage> {
		const d = await this.gql<{ sourceBrowse: SourceBrowsePage }>(ops.SOURCE_BROWSE, {
			sourceId,
			type,
			page,
			query: query ?? null,
		});
		return d.sourceBrowse;
	}

	async addExtensionRepo(indexUrl: string): Promise<number> {
		const d = await this.gql<{ addExtensionRepo: number }>(ops.ADD_EXTENSION_REPO, { indexUrl });
		return d.addExtensionRepo;
	}

	async installExtension(pkgName: string): Promise<ExtensionInfo> {
		const d = await this.gql<{ installExtension: ExtensionInfo }>(ops.INSTALL_EXTENSION, {
			pkgName,
		});
		return d.installExtension;
	}

	async uninstallExtension(pkgName: string): Promise<ExtensionInfo> {
		const d = await this.gql<{ uninstallExtension: ExtensionInfo }>(ops.UNINSTALL_EXTENSION, {
			pkgName,
		});
		return d.uninstallExtension;
	}

	async updateExtension(pkgName: string): Promise<ExtensionInfo> {
		const d = await this.gql<{ updateExtension: ExtensionInfo }>(ops.UPDATE_EXTENSION, {
			pkgName,
		});
		return d.updateExtension;
	}

	async bulkAddSourceSeries(suwayomiMangaIds: Id[]): Promise<BulkAddResult> {
		const d = await this.gql<{ bulkAddSourceSeries: BulkAddResult }>(ops.BULK_ADD_SOURCE_SERIES, {
			suwayomiMangaIds,
		});
		return d.bulkAddSourceSeries;
	}

	async seriesSourcesBatch(seriesIds: Id[]): Promise<SeriesSourceGroup[]> {
		const d = await this.gql<{ seriesSourcesBatch: SeriesSourceGroup[] }>(
			ops.SERIES_SOURCES_BATCH,
			{ seriesIds },
		);
		return d.seriesSourcesBatch;
	}

	async setSeriesPaused(seriesId: Id, paused: boolean): Promise<Series> {
		const d = await this.gql<{ setSeriesPaused: Series }>(ops.SET_SERIES_PAUSED, {
			seriesId,
			paused,
		});
		return d.setSeriesPaused;
	}

	async sourceIngestJobs(active = false): Promise<SourceIngestJob[]> {
		const d = await this.gql<{ sourceIngestJobs: SourceIngestJob[] }>(ops.SOURCE_INGEST_JOBS, {
			active,
		});
		return d.sourceIngestJobs;
	}

	async startSourceIngest(sourceId: Id): Promise<SourceIngestJob> {
		const d = await this.gql<{ startSourceIngest: SourceIngestJob }>(ops.START_SOURCE_INGEST, {
			sourceId,
		});
		return d.startSourceIngest;
	}

	async cancelSourceIngest(jobId: Id): Promise<SourceIngestJob> {
		const d = await this.gql<{ cancelSourceIngest: SourceIngestJob }>(ops.CANCEL_SOURCE_INGEST, {
			jobId,
		});
		return d.cancelSourceIngest;
	}

	async startExtensionIngest(pkgName: Id): Promise<SourceIngestJob[]> {
		const d = await this.gql<{ startExtensionIngest: SourceIngestJob[] }>(
			ops.START_EXTENSION_INGEST,
			{ pkgName },
		);
		return d.startExtensionIngest;
	}

	async cancelExtensionIngest(pkgName: Id): Promise<SourceIngestJob[]> {
		const d = await this.gql<{ cancelExtensionIngest: SourceIngestJob[] }>(
			ops.CANCEL_EXTENSION_INGEST,
			{ pkgName },
		);
		return d.cancelExtensionIngest;
	}

	// --- admin maintenance ---
	async persistCatalogue(): Promise<number> {
		const d = await this.gql<{ persistCatalogue: number }>(ops.PERSIST_CATALOGUE);
		return d.persistCatalogue;
	}

	// --- viewer preferences ---
	async setShowNsfw(value: boolean): Promise<boolean> {
		const d = await this.gql<{ setShowNsfw: boolean }>(ops.SET_SHOW_NSFW, { value });
		return d.setShowNsfw;
	}

	// --- profile ---
	async updateProfile(input: UpdateProfileInput): Promise<Session['user']> {
		const d = await this.gql<{ updateProfile: Session['user'] }>(ops.UPDATE_PROFILE, { input });
		return d.updateProfile;
	}
	async myActivity(limit = 20): Promise<Activity[]> {
		const d = await this.gql<{ myActivity: Activity[] }>(ops.MY_ACTIVITY, { limit });
		return d.myActivity;
	}
	/**
	 * Avatar upload is a REST multipart POST (not GraphQL): the endpoint is the
	 * API origin's `/avatar`, derived from the GraphQL `endpoint` by dropping the
	 * trailing `/graphql`. Returns the new avatar URL (a `/avatars/...` path,
	 * resolved against the API origin by the reader).
	 */
	async uploadAvatar(file: Blob): Promise<string> {
		const doFetch = this.config.fetch ?? fetch;
		const url = this.config.endpoint.replace(/\/graphql\/?$/, '') + '/avatar';
		const form = new FormData();
		form.append('avatar', file);
		const res = await doFetch(url, {
			method: 'POST',
			headers: this.config.token ? { authorization: `Bearer ${this.config.token}` } : {},
			body: form,
		});
		const json = (await res.json().catch(() => null)) as {
			avatarUrl?: string;
			message?: string;
		} | null;
		if (!res.ok) throw new Error(json?.message ?? `Upload failed (${res.status})`);
		if (!json?.avatarUrl) throw new Error('Upload returned no avatar URL');
		return json.avatarUrl;
	}

	// --- canonical catalogue ---
	async canonicalUpdates(page = 1): Promise<CanonicalUpdate[]> {
		const d = await this.gql<{ canonicalUpdates: CanonicalUpdate[] }>(ops.CANONICAL_UPDATES, {
			page,
		});
		return d.canonicalUpdates;
	}

	// --- canonical reader path ---
	async canonicalSeries(workId: Id): Promise<Series> {
		const d = await this.gql<{ canonicalSeries: Series }>(ops.CANONICAL_SERIES, { workId });
		return d.canonicalSeries;
	}
	async canonicalChapters(workId: Id): Promise<Chapter[]> {
		const d = await this.gql<{ canonicalChapters: Chapter[] }>(ops.CANONICAL_CHAPTERS, { workId });
		return d.canonicalChapters;
	}
	async canonicalPages(chapterId: Id): Promise<Page[]> {
		const d = await this.gql<{ canonicalPages: Page[] }>(ops.CANONICAL_PAGES, { chapterId });
		return d.canonicalPages;
	}

	// --- native-embedded-Suwayomi source routing ---
	async workSources(workId: Id): Promise<WorkSource[]> {
		const d = await this.gql<{ workSources: WorkSource[] }>(ops.WORK_SOURCES, { workId });
		return d.workSources;
	}
	async workSourcesBatch(workIds: Id[]): Promise<WorkSourceGroup[]> {
		const d = await this.gql<{ workSourcesBatch: WorkSourceGroup[] }>(ops.WORK_SOURCES_BATCH, {
			workIds,
		});
		return d.workSourcesBatch;
	}
}

export function createBackend(config: BackendConfig): Backend {
	return new GraphQLBackend(config);
}
