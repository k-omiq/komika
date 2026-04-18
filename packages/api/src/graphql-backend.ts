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
} from '@komika/types';
import type {
	Activity,
	Backend,
	PostCommentInput,
	PostReviewInput,
	RegisterInput,
	SeriesAdminInput,
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
	async search(query: string, page = 1): Promise<Paginated<Series>> {
		const d = await this.gql<{ search: Paginated<Series> }>(ops.SEARCH, { query, page });
		return d.search;
	}
	async series(id: Id): Promise<Series> {
		const d = await this.gql<{ series: Series }>(ops.SERIES, { id });
		return d.series;
	}
	async chapters(seriesId: Id): Promise<Chapter[]> {
		const d = await this.gql<{ chapters: Chapter[] }>(ops.CHAPTERS, { seriesId });
		return d.chapters;
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
	async library(): Promise<Series[]> {
		const d = await this.gql<{ library: Series[] }>(ops.LIBRARY);
		return d.library;
	}
	async setProgress(chapterId: Id, lastPageRead: number, read: boolean): Promise<void> {
		await this.gql<{ setProgress: boolean }>(ops.SET_PROGRESS, { chapterId, lastPageRead, read });
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
	async postComment(input: PostCommentInput): Promise<Comment> {
		const d = await this.gql<{ postComment: Comment }>(ops.POST_COMMENT, { input });
		return d.postComment;
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
		const json = (await res.json().catch(() => null)) as { avatarUrl?: string; message?: string } | null;
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
}

export function createBackend(config: BackendConfig): Backend {
	return new GraphQLBackend(config);
}
