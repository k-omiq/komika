import type {
	Chapter,
	ChapterComment,
	DiscoveryFeed,
	Id,
	Page,
	Paginated,
	Review,
	ScanStatus,
	Series,
} from '@komika/types';
import type {
	Backend,
	PostCommentInput,
	PostReviewInput,
	RegisterInput,
	SeriesAdminInput,
	Session,
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
	async postReview(input: PostReviewInput): Promise<Review> {
		const d = await this.gql<{ postReview: Review }>(ops.POST_REVIEW, { input });
		return d.postReview;
	}
	async comments(chapterId: Id, page = 1): Promise<Paginated<ChapterComment>> {
		const d = await this.gql<{ comments: Paginated<ChapterComment> }>(ops.COMMENTS, {
			chapterId,
			page,
		});
		return d.comments;
	}
	async postComment(input: PostCommentInput): Promise<ChapterComment> {
		const d = await this.gql<{ postComment: ChapterComment }>(ops.POST_COMMENT, { input });
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
}

export function createBackend(config: BackendConfig): Backend {
	return new GraphQLBackend(config);
}
