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
import type { ContentBackend, SourceRef } from './content-backend.js';
import type { WorkSourceLike } from './local-suwayomi-backend.js';

/**
 * The optional source-ref mapping capability layered on the local content backend by
 * {@link LocalSuwayomiBackend}. `ContentBackend` intentionally stays mapping-agnostic
 * (it takes a resolved `SourceRef`), so the composite feature-detects `refFor` on the
 * concrete local backend to translate a hosted `WorkSource` into an engine `SourceRef`.
 */
interface RefMapper {
	refFor(ws: WorkSourceLike, title?: string): Promise<SourceRef | null>;
}

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

	/**
	 * Reconciliation map from a hosted canonical chapter id (a globally-unique uuid) to
	 * the embedded engine's integer chapter id (as a string), populated during
	 * {@link canonicalChapters} and consulted by {@link canonicalPages} to serve page
	 * bytes on-device. Keyed flat by canonical chapter id — safe because canonical
	 * chapter ids are globally unique, and it keeps `canonicalPages` (which has no
	 * workId) a single lookup. The engine id NEVER reaches `setProgress`: only page
	 * image bytes are served locally; chapter identity/list/progress stay canonical.
	 */
	private readonly localChapterMap = new Map<string, string>();

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
	series(id: Id): Promise<Series> {
		// Always hosted. Local serving for canonical works happens in the `canonical*`
		// overrides — the reader routes `w_` canonical ids there. These plain methods
		// are only hit for non-canonical numeric/source ids, which the native+canonical
		// path never produces, so there is no on-device branch to take here.
		return this.opts.hosted.series(id);
	}
	chapters(seriesId: Id): Promise<Chapter[]> {
		// Always hosted — see `series` above. Canonical chapter reconciliation (and any
		// on-device serving) lives in `canonicalChapters` / `canonicalPages`.
		return this.opts.hosted.chapters(seriesId);
	}
	pages(chapterId: Id): Promise<Page[]> {
		// Always hosted — see `series` above. On-device page bytes for canonical works
		// are served by `canonicalPages`, not here.
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
	/**
	 * The hosted canonical chapter list is ALWAYS the authoritative return — its uuid
	 * ids, order, and shape are untouched, so the reader's `setProgress(chapter.id)`
	 * keeps routing canonical uuids to canonical progress. As a best-effort side effect,
	 * when the local engine is ready this reconciles the hosted chapters against the
	 * engine's live chapters BY NUMBER (D7) and records `canonicalChapterId → engineChapterId`
	 * in {@link localChapterMap}, which {@link canonicalPages} later consults to serve
	 * page bytes on-device. Any local failure is swallowed — reconciliation never
	 * changes the returned list and never hard-fails the read.
	 */
	async canonicalChapters(workId: Id): Promise<Chapter[]> {
		const chs = await this.opts.hosted.canonicalChapters!(workId);
		if (await this.localReady()) {
			try {
				const sources = await this.opts.hosted.workSources!(workId);
				// MangaDex-preferred is already first; require a usable mapping (MangaDex, or
				// a suwayomi source carrying extension provisioning coords).
				const ws = sources.find((s) => s.sourceType === 'mangadex' || s.extension != null);
				const mapper = this.opts.local as unknown as Partial<RefMapper>;
				if (ws && typeof mapper.refFor === 'function') {
					// Title hint is skipped: MangaDex resolves by uuid without it (a Keiyoushi
					// title-search fallback would cost an extra canonicalSeries round-trip).
					const ref = await mapper.refFor(ws);
					if (ref) {
						const engineChs = await this.opts.local!.chapters(ref);
						for (const ch of chs) {
							const matches = engineChs.filter((e) => Math.abs(e.number - ch.number) < 1e-6);
							if (matches.length === 0) continue;
							// Disambiguate same-number engine chapters by scanlator, else take the first.
							const pick =
								matches.length > 1
									? (matches.find((e) => e.scanlator != null && e.scanlator === ch.scanlator) ??
										matches[0])
									: matches[0];
							this.localChapterMap.set(ch.id, String(pick.id));
						}
					}
				}
			} catch (err) {
				// Best-effort only: leave the map as-is; canonicalPages falls back to hosted.
				console.warn('[composite] local chapter reconciliation failed:', err);
			}
		}
		return chs;
	}
	/**
	 * Page image bytes are served live from the embedded engine when this canonical
	 * chapter was reconciled to an engine chapter (see {@link canonicalChapters}) and
	 * the engine is ready; on ANY local error we fall through to the hosted proxy. The
	 * chapter id here is the canonical uuid, looked up in {@link localChapterMap} to the
	 * engine's integer chapter id — the engine id never leaks back to identity/progress.
	 */
	async canonicalPages(chapterId: Id): Promise<Page[]> {
		if ((await this.localReady()) && this.localChapterMap.has(chapterId)) {
			try {
				return await this.opts.local!.pages(this.localChapterMap.get(chapterId)!);
			} catch (err) {
				// Fall through to hosted on any local failure — never hard-fail the read.
				console.warn('[composite] local canonicalPages failed, using hosted:', err);
			}
		}
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
