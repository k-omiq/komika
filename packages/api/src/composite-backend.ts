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
	WorkSource,
	WorkSourceGroup,
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
import type { ContentBackend } from './content-backend.js';

/**
 * A {@link Backend} that splits work between a hosted server and an optional
 * on-device content engine. Auth, library, reading-progress, the social layer,
 * admin, canonical mirror, and source routing ALWAYS go to the hosted backend;
 * only live series/chapters/pages content is eligible to be served locally (see
 * the §1 routing table + §10 in `docs/plans/native-embedded-suwayomi.md`).
 *
 * In Wave B there is no local engine (its transport lands in Wave C), so
 * `local` is null and every call — including the three content methods —
 * delegates to `hosted`, making this wrapper behaviorally identical to the plain
 * hosted backend. When Wave C wires a real {@link ContentBackend}, the three
 * content branches resolve a source mapping and fetch on-device instead.
 */
export class CompositeBackend implements Backend {
	constructor(private opts: { hosted: Backend; local?: ContentBackend | null }) {}

	/** Whether the local content engine exists and reports itself ready right now. */
	private async localReady(): Promise<boolean> {
		return !!this.opts.local && (await this.opts.local.isReady());
	}

	// --- auth ---
	session(): Promise<Session | null> {
		return this.opts.hosted.session();
	}
	login(username: string, password: string): Promise<Session> {
		return this.opts.hosted.login(username, password);
	}
	register(input: RegisterInput): Promise<Session> {
		return this.opts.hosted.register(input);
	}
	logout(): Promise<void> {
		return this.opts.hosted.logout();
	}
	/** Forward the bearer token to the hosted backend only — the local engine is
	 * content-only and never receives a token. */
	setToken(token: string | null): void {
		this.opts.hosted.setToken?.(token);
	}

	// --- discovery / catalog ---
	discovery(): Promise<DiscoveryFeed[]> {
		return this.opts.hosted.discovery();
	}
	updates(page?: number): Promise<Paginated<Series>> {
		return this.opts.hosted.updates(page);
	}
	search(query: string, page?: number): Promise<Paginated<Series>> {
		return this.opts.hosted.search(query, page);
	}
	async series(id: Id): Promise<Series> {
		// Wave C: when the local engine is ready, resolve (sourceId, sourceKey) for this
		// work via workSources and fetch the live metadata from `this.opts.local`. Until
		// that transport lands, the local branch is inert and we always use the hosted server.
		if (await this.localReady()) {
			// TODO(Wave C): return this.opts.local!.series(ref)
		}
		return this.opts.hosted.series(id);
	}
	async chapters(seriesId: Id): Promise<Chapter[]> {
		// Wave C: when the local engine is ready, resolve (sourceId, sourceKey) for this
		// work via workSources and fetch the live list from `this.opts.local`. Until that
		// transport lands, the local branch is inert and we always use the hosted server.
		if (await this.localReady()) {
			// TODO(Wave C): return this.opts.local!.chapters(ref)
		}
		return this.opts.hosted.chapters(seriesId);
	}
	async pages(chapterId: Id): Promise<Page[]> {
		// Wave C: when the local engine is ready, fetch the page image URLs on-device from
		// `this.opts.local` using the source-local chapter id. Until that transport lands,
		// the local branch is inert and we always use the hosted server.
		if (await this.localReady()) {
			// TODO(Wave C): return this.opts.local!.pages(chapterId)
		}
		return this.opts.hosted.pages(chapterId);
	}

	// --- library & progress ---
	mark(seriesId: Id, marked: boolean): Promise<Series> {
		return this.opts.hosted.mark(seriesId, marked);
	}
	library(): Promise<Series[]> {
		return this.opts.hosted.library();
	}
	setProgress(chapterId: Id, lastPageRead: number, read: boolean): Promise<void> {
		return this.opts.hosted.setProgress(chapterId, lastPageRead, read);
	}

	// --- viewer preferences ---
	setShowNsfw(value: boolean): Promise<boolean> {
		return this.opts.hosted.setShowNsfw!(value);
	}

	// --- profile ---
	updateProfile(input: UpdateProfileInput): Promise<Session['user']> {
		return this.opts.hosted.updateProfile!(input);
	}
	uploadAvatar(file: Blob): Promise<string> {
		return this.opts.hosted.uploadAvatar!(file);
	}
	myActivity(limit?: number): Promise<Activity[]> {
		return this.opts.hosted.myActivity!(limit);
	}

	// --- canonical catalogue (MangaDex mirror) ---
	canonicalUpdates(page?: number): Promise<CanonicalUpdate[]> {
		return this.opts.hosted.canonicalUpdates!(page);
	}
	canonicalSeries(workId: Id): Promise<Series> {
		return this.opts.hosted.canonicalSeries!(workId);
	}
	canonicalChapters(workId: Id): Promise<Chapter[]> {
		return this.opts.hosted.canonicalChapters!(workId);
	}
	canonicalPages(chapterId: Id): Promise<Page[]> {
		return this.opts.hosted.canonicalPages!(chapterId);
	}
	workSources(workId: Id): Promise<WorkSource[]> {
		return this.opts.hosted.workSources!(workId);
	}
	workSourcesBatch(workIds: Id[]): Promise<WorkSourceGroup[]> {
		return this.opts.hosted.workSourcesBatch!(workIds);
	}

	// --- social ---
	reviews(seriesId: Id, page?: number): Promise<Paginated<Review>> {
		return this.opts.hosted.reviews(seriesId, page);
	}
	myReview(seriesId: Id): Promise<Review | null> {
		return this.opts.hosted.myReview!(seriesId);
	}
	postReview(input: PostReviewInput): Promise<Review> {
		return this.opts.hosted.postReview(input);
	}
	comments(targetType: CommentTargetType, targetId: Id, page?: number): Promise<Paginated<Comment>> {
		return this.opts.hosted.comments(targetType, targetId, page);
	}
	postComment(input: PostCommentInput): Promise<Comment> {
		return this.opts.hosted.postComment(input);
	}

	// --- admin "manga DB" ---
	updateSeriesAdmin(input: SeriesAdminInput): Promise<Series> {
		return this.opts.hosted.updateSeriesAdmin!(input);
	}
	scanStatus(): Promise<ScanStatus> {
		return this.opts.hosted.scanStatus!();
	}
	triggerScan(seriesId: Id): Promise<Series> {
		return this.opts.hosted.triggerScan!(seriesId);
	}

	// --- admin moderation ---
	banUser(userId: Id, banned: boolean): Promise<AdminUser> {
		return this.opts.hosted.banUser!(userId, banned);
	}
	deleteComment(commentId: Id): Promise<boolean> {
		return this.opts.hosted.deleteComment!(commentId);
	}

	// --- admin user management ---
	users(page?: number): Promise<Paginated<AdminUser>> {
		return this.opts.hosted.users!(page);
	}
	setUserAdmin(userId: Id, isAdmin: boolean): Promise<AdminUser> {
		return this.opts.hosted.setUserAdmin!(userId, isAdmin);
	}

	// --- admin dedup review ---
	mergeQueue(): Promise<MergeCandidate[]> {
		return this.opts.hosted.mergeQueue!();
	}
	resolveMergeCandidate(id: Id, accept: boolean): Promise<boolean> {
		return this.opts.hosted.resolveMergeCandidate!(id, accept);
	}
	addSourceSeries(suwayomiMangaId: Id): Promise<MatchResult> {
		return this.opts.hosted.addSourceSeries!(suwayomiMangaId);
	}
}

/** Build a {@link CompositeBackend} from a hosted backend and an optional local
 * content engine (mirrors {@link createBackend}). */
export function createCompositeBackend(opts: {
	hosted: Backend;
	local?: ContentBackend | null;
}): Backend {
	return new CompositeBackend(opts);
}
