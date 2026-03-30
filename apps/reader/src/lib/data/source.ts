/**
 * The data repository the UI consumes (via SvelteKit `load` functions).
 *
 * Each getter returns the exact view shapes the screens render. When the backend
 * is enabled (`PUBLIC_KOMIKA_BACKEND=on`) it maps live `@komika/api` domain data
 * into those shapes; otherwise — or if a live call throws — it falls back to
 * `mock.ts`, so the app is always renderable. This is the single seam to swap as
 * the backend fills in: screens never import `mock.ts` directly.
 */
import type {
	CanonicalUpdate,
	Chapter,
	ComicType as DomainComicType,
	Series,
	SeriesStatus,
} from '@komika/types';
import { backend, images } from '$lib/context';
import { config } from '$lib/config';
import * as mock from './mock';
import type { Card, CatalogEntry, ComicType, Status } from './mock';

const LIVE = config.backendEnabled;

/**
 * Whether a series id addresses a canonical (MangaDex-mirrored) `work` rather than a
 * numeric Suwayomi series. Canonical work ids carry a `w_` prefix (CATALOGUE.md §6),
 * so the reader routes them to the `canonical*` backend methods; Suwayomi ids are
 * numeric and stay on the untouched Suwayomi path.
 */
function isCanonicalId(id: string): boolean {
	return id.startsWith('w_');
}

/** Run a live mapping, falling back to mock on any failure. */
async function live<T>(fn: () => Promise<T>, fallback: T): Promise<T> {
	if (!LIVE) return fallback;
	try {
		return await fn();
	} catch (err) {
		console.warn('[komika] backend call failed, using mock fallback:', err);
		return fallback;
	}
}

// ---- domain → view mappers -------------------------------------------------

function toViewType(t: DomainComicType): ComicType {
	if (t === 'MANHWA' || t === 'WEBTOON') return 'Manhwa';
	if (t === 'MANHUA') return 'Manhua';
	return 'Manga';
}

function toViewStatus(s: Series['status']): Status {
	if (s === 'COMPLETED') return 'completed';
	if (s === 'HIATUS') return 'hiatus';
	return 'ongoing';
}

function relTime(iso: string | null | undefined): string {
	if (!iso) return '';
	const then = Date.parse(iso);
	if (Number.isNaN(then)) return '';
	// `Date.now()` is unavailable in some sandboxes; guard it.
	const now = typeof Date !== 'undefined' && Date.now ? Date.now() : then;
	const mins = Math.max(0, Math.round((now - then) / 60000));
	if (mins < 60) return `${mins}m`;
	const hrs = Math.round(mins / 60);
	if (hrs < 24) return `${hrs}h`;
	return `${Math.round(hrs / 24)}d`;
}

function toCard(s: Series): Card {
	return {
		title: s.title,
		ch: `Ch. ${s.chapterCount}`,
		time: relTime(s.updatedAt),
		rating: s.rating.average.toFixed(1),
		cover: s.coverUrl,
		id: s.id,
	};
}

/** A canonical-updates row → home/updates Card, linking by its `w_` workId so the
 *  card opens the MangaDex-mirrored work through the canonical reader path. */
function toCanonicalCard(u: CanonicalUpdate): Card {
	return {
		title: u.title ?? 'Untitled',
		ch: u.latestChapter ? `Ch. ${u.latestChapter}` : '',
		time: relTime(u.latestAt),
		rating: '',
		cover: u.coverUrl ?? '',
		id: u.workId,
	};
}

function toCatalogEntry(s: Series, i: number): CatalogEntry {
	return {
		title: s.title,
		author: s.author ?? '',
		genre: s.genres[0] ?? '',
		ch: s.chapterCount,
		rating: s.rating.average,
		status: toViewStatus(s.status),
		added: i,
		type: toViewType(s.type),
		cover: s.coverUrl,
		id: s.id,
	};
}

/** Dedupe a series list by id, preserving first-seen order. */
function dedupeSeries(list: Series[]): Series[] {
	const seen = new Set<string>();
	const out: Series[] = [];
	for (const s of list) {
		if (!seen.has(s.id)) {
			seen.add(s.id);
			out.push(s);
		}
	}
	return out;
}

/** Home hero slide — carries cover + id so the hero can render art and link. */
export interface FeaturedView {
	title: string;
	genre: string;
	ch: number;
	cover: string;
	id?: string;
}

function toFeatured(s: Series): FeaturedView {
	return {
		title: s.title,
		genre: s.genres[0] ?? '',
		ch: s.chapterCount,
		cover: s.coverUrl,
		id: s.id,
	};
}

/**
 * Per-format counts backed by the live catalog sample (the union of discovery
 * feeds), keeping the mock cards' presentation metadata. Not the true global
 * total — the federated catalog isn't fully enumerated client-side — but a real
 * reflection of what's currently surfaced.
 */
function deriveFormatCards(pool: Series[]): typeof mock.formatCards {
	return mock.formatCards.map((card) => {
		const n = pool.filter((s) => toViewType(s.type) === card.type).length;
		return { ...card, count: `${n} ${n === 1 ? 'title' : 'titles'}` };
	});
}

/** The most common genres across the live catalog sample, most-frequent first. */
function deriveGenres(pool: Series[]): string[] {
	const freq = new Map<string, number>();
	for (const s of pool) for (const g of s.genres) freq.set(g, (freq.get(g) ?? 0) + 1);
	return [...freq.entries()]
		.sort((a, b) => b[1] - a[1])
		.slice(0, 9)
		.map(([g]) => g);
}

/** Classify a series onto a library shelf from its real read progress. */
function shelfFor(read: number, total: number): 'reading' | 'completed' | 'plan' {
	if (total > 0 && read >= total) return 'completed';
	if (read === 0) return 'plan';
	return 'reading';
}

// ---- per-screen getters ----------------------------------------------------

export function getHome() {
	const fallback = {
		featured: mock.featured.map((f): FeaturedView => ({ ...f, cover: '' })),
		latestUpdates: mock.latestUpdates,
		trending: mock.trending,
		latestAdded: mock.latestAdded,
		formatCards: mock.formatCards,
		homeGenres: mock.homeGenres,
	};
	return live(async () => {
		// Discovery drives the curated rows; the scanner-backed `updates` feed drives
		// "Latest Updates" (not Suwayomi's source "Latest").
		const [feeds, updates] = await Promise.all([
			backend.discovery(),
			backend.updates().catch(() => ({ items: [] as Series[] })),
		]);
		const byKind = (k: string) => feeds.find((f) => f.kind === k)?.items ?? [];
		const popular = byKind('POPULAR');
		const pool = dedupeSeries(feeds.flatMap((f) => f.items));
		const featured = (popular.length ? popular : (feeds[0]?.items ?? []))
			.slice(0, 5)
			.map(toFeatured);
		return {
			featured: featured.length ? featured : fallback.featured,
			latestUpdates: updates.items.map(toCard),
			trending: byKind('TRENDING').map(toCard),
			latestAdded: byKind('RECENTLY_ADDED').map(toCard),
			formatCards: pool.length ? deriveFormatCards(pool) : mock.formatCards,
			homeGenres: pool.length ? deriveGenres(pool) : mock.homeGenres,
		};
	}, fallback);
}

export function getBrowseCatalog(): Promise<CatalogEntry[]> {
	return live(async () => {
		const { items } = await backend.search('');
		return items.map(toCatalogEntry);
	}, mock.catalog);
}

export function getUpdates() {
	const fallback = {
		trendingGroups: mock.trendingGroups,
		newUpdates: mock.newUpdates,
		hotUpdates: mock.hotUpdates,
	};
	return live(async () => {
		// "New" is the scanner-driven Updates feed (series with freshly-detected
		// chapters, newest-first). "Trending"/"Hot" reuse the discovery Trending
		// feed. Empty (no detections yet) renders the page's empty state rather than
		// silently substituting mock — mock is only the off/error fallback.
		const [feeds, updates, canonical] = await Promise.all([
			backend.discovery(),
			backend.updates(),
			// Canonical (MangaDex-mirrored) updates — openable via their `w_` workId
			// through the canonical reader path. Optional method; empty on failure/off.
			backend.canonicalUpdates?.().catch(() => [] as CanonicalUpdate[]) ??
				Promise.resolve([] as CanonicalUpdate[]),
		]);
		const byKind = (k: string) => feeds.find((f) => f.kind === k)?.items ?? [];
		const trending = byKind('TRENDING').map(toCard);
		const recent = updates.items.map(toCard);
		const canonicalCards = canonical.map(toCanonicalCard);
		return {
			trendingGroups: trending.length ? [{ label: 'Trending Today', items: trending }] : [],
			// Scanner detections first, then the MangaDex mirror's freshest works.
			newUpdates: [...recent, ...canonicalCards],
			hotUpdates: trending,
		};
	}, fallback);
}

export function getLibrary() {
	const fallback = {
		libraryCatalog: mock.libraryCatalog.map((c) => ({ ...c, id: undefined as string | undefined })),
		continueRow: mock.continueRow.map((c) => ({ ...c, id: undefined as string | undefined })),
	};
	return live(async () => {
		const lib = await backend.library();
		// Per-series read progress from real chapter state (one fetch per series —
		// fine for a modest library).
		const rows = await Promise.all(
			lib.map(async (s) => {
				const chs = await backend.chapters(s.id).catch(() => [] as Chapter[]);
				const total = chs.length || s.chapterCount;
				const read = chs.filter((c) => c.read).length;
				return { s, chs, total, read };
			}),
		);
		const libraryCatalog = rows.map(({ s, total, read }) => ({
			id: s.id,
			title: s.title,
			genre: s.genres[0] ?? '',
			rating: s.rating.average.toFixed(1),
			shelf: shelfFor(read, total),
			read,
			total,
		}));
		// Continue-reading: series with progress underway, next unread chapter first.
		const continueRow = rows
			.filter(({ read, total }) => read > 0 && read < total)
			.map(({ s, chs, read, total }) => {
				const asc = [...chs].sort((a, b) => a.number - b.number);
				const next = asc.find((c) => !c.read) ?? asc[asc.length - 1];
				return {
					id: s.id,
					title: s.title,
					ch: `Ch. ${next?.number ?? read + 1}`,
					progress: total ? Math.round((read / total) * 100) : 0,
					genre: s.genres[0] ?? '',
				};
			});
		return { libraryCatalog, continueRow };
	}, fallback);
}

// ---- series detail ---------------------------------------------------------

export interface SeriesChapterView {
	id?: string;
	n: number;
	title: string;
	date: string;
	isNew: boolean;
	read: boolean;
}
export interface SeriesDetailView {
	id: string;
	title: string;
	type: ComicType;
	flag: string;
	rating: string;
	votes: string;
	totalCh: number;
	updated: string;
	statusLabel: string;
	author: string;
	artist: string;
	genres: string[];
	synopsis: string;
	cover: string;
	continueCh: number;
	startChapterId?: string;
	isMarked: boolean;
}
export interface RelatedView {
	title: string;
	genre: string;
	ch: number;
	rating: string;
	cover?: string;
	id?: string;
}
export interface SeriesView {
	detail: SeriesDetailView;
	chapters: SeriesChapterView[];
	related: RelatedView[];
}

const STATUS_WORD: Record<SeriesStatus, string> = {
	ONGOING: 'ONGOING',
	COMPLETED: 'COMPLETED',
	HIATUS: 'HIATUS',
	CANCELLED: 'CANCELLED',
	UNKNOWN: 'ONGOING',
};

function seriesFallback(id: string): SeriesView {
	const totalCh = mock.seriesDetail.chapters;
	const readUpTo = Math.min(totalCh, Math.round(totalCh * 0.427));
	return {
		detail: {
			id,
			title: mock.seriesDetail.title,
			type: 'Manga',
			flag: mock.FLAG.Manga,
			rating: mock.seriesDetail.rating,
			votes: mock.seriesDetail.votes,
			totalCh,
			updated: mock.seriesDetail.updated,
			statusLabel: mock.seriesDetail.status,
			author: mock.seriesDetail.author,
			artist: mock.seriesDetail.artist,
			genres: mock.seriesDetail.genres,
			synopsis: mock.seriesDetail.synopsis,
			cover: '',
			continueCh: Math.min(totalCh, readUpTo + 1),
			startChapterId: undefined,
			isMarked: false,
		},
		chapters: mock.buildChapters(totalCh, readUpTo).map((c) => ({
			n: c.n,
			title: c.title,
			date: c.date,
			isNew: c.isNew,
			read: c.read,
		})),
		related: mock.relatedSeries.map((r) => ({ ...r })),
	};
}

function toRelated(p: Series): RelatedView {
	return {
		title: p.title,
		genre: p.genres[0] ?? '',
		ch: p.chapterCount,
		rating: p.rating.average.toFixed(1),
		cover: p.coverUrl,
		id: p.id,
	};
}

/** Pick related series from a candidate pool, preferring shared genres. */
function relatedFor(s: Series, pool: Series[]): RelatedView[] {
	const others = pool.filter((p) => p.id !== s.id);
	const shared = others.filter((p) => p.genres.some((g) => s.genres.includes(g)));
	const picks = (shared.length ? shared : others).slice(0, 8);
	return picks.map(toRelated);
}

function mapSeriesView(
	s: Series,
	chs: Chapter[],
	pool: Series[],
	preserveOrder = false,
): SeriesView {
	const type = toViewType(s.type);
	// Canonical chapters are already server-ordered ascending (number-less last); sorting
	// by number would float a oneshot (wire value 0) ahead of ch. 1 (CR4). The Suwayomi
	// path keeps its explicit ascending sort.
	const asc = preserveOrder ? chs : [...chs].sort((a, b) => a.number - b.number);
	const firstUnread = asc.find((c) => !c.read) ?? asc[0];
	return {
		detail: {
			id: s.id,
			title: s.title,
			type,
			flag: mock.FLAG[type],
			rating: s.rating.average.toFixed(1),
			votes: String(s.rating.count),
			totalCh: s.chapterCount || chs.length,
			updated: relTime(s.updatedAt) || 'recently',
			statusLabel: STATUS_WORD[s.status],
			author: s.author ?? '',
			artist: s.artist ?? '',
			genres: s.genres,
			synopsis: s.description ?? '',
			cover: s.coverUrl,
			continueCh: firstUnread?.number ?? 1,
			startChapterId: firstUnread?.id,
			isMarked: s.isMarked,
		},
		chapters: chs.map((c, i) => ({
			id: c.id,
			n: c.number,
			title: c.title || `Chapter ${c.number}`,
			date: relTime(c.uploadedAt),
			isNew: i < 3,
			read: c.read,
		})),
		related: relatedFor(s, pool),
	};
}

export function getSeries(id: string): Promise<SeriesView> {
	return live(async () => {
		// Canonical (MangaDex-mirrored) works resolve through the canonical path; there
		// is no genre-related pool for them yet, so related is empty.
		if (isCanonicalId(id) && backend.canonicalSeries && backend.canonicalChapters) {
			const [s, chs] = await Promise.all([
				backend.canonicalSeries(id),
				backend.canonicalChapters(id),
			]);
			return mapSeriesView(s, chs, [], true);
		}
		// Fetch a candidate pool (Popular) alongside the series to seed related-by-
		// genre. A pool failure just yields no related — it never fails the page.
		const [s, chs, pool] = await Promise.all([
			backend.series(id),
			backend.chapters(id),
			backend
				.search('')
				.then((r) => r.items)
				.catch(() => [] as Series[]),
		]);
		return mapSeriesView(s, chs, pool);
	}, seriesFallback(id));
}

/**
 * Add/remove a series from the library. Returns the resulting marked state
 * (the backend's `isMarked`), or the optimistic value when offline/mock so the
 * UI toggle still responds.
 */
export async function setLibraryMark(seriesId: string, marked: boolean): Promise<boolean> {
	if (!LIVE) return marked;
	try {
		const s = await backend.mark(seriesId, marked);
		return s.isMarked;
	} catch (err) {
		console.warn('[komika] mark failed:', err);
		return marked;
	}
}

/** Persist reading progress for a chapter (best-effort; no-op offline/mock). */
export async function saveProgress(
	chapterId: string | undefined,
	lastPageRead: number,
	read: boolean,
): Promise<void> {
	if (!LIVE || !chapterId) return;
	// Both numeric Suwayomi chapter ids and MangaDex-uuid canonical ids persist:
	// the server routes on id shape (canonical → `canonical_progress`) (CR6). The
	// non-empty guard above is the only shape check the client needs.
	try {
		await backend.setProgress(chapterId, lastPageRead, read);
	} catch (err) {
		console.warn('[komika] setProgress failed:', err);
	}
}

// ---- reader ----------------------------------------------------------------

export interface ReaderPageView {
	url: string; // resolved displayable URL ('' → render placeholder)
	index: number;
	ratio: string;
	label: string;
	dim: string;
}
export interface ReaderChapterRef {
	id?: string;
	n: number;
	title: string;
}
export interface ReaderView {
	seriesId: string;
	seriesTitle: string;
	chapterId?: string;
	chNum: number;
	chTitle: string;
	pages: ReaderPageView[];
	chapters: ReaderChapterRef[];
	prevChapterId?: string;
	nextChapterId?: string;
}

function readerFallback(seriesId: string): ReaderView {
	const chNum = 62;
	return {
		seriesId,
		seriesTitle: 'Vermilion Hours',
		chapterId: undefined,
		chNum,
		chTitle: mock.chapterTitle(chNum),
		pages: mock.readerPages().map((p) => ({
			url: '',
			index: p.n - 1,
			ratio: p.ratio,
			label: p.label,
			dim: p.dim,
		})),
		chapters: Array.from({ length: 8 }, (_, k) => {
			const n = chNum + 2 - k;
			return { n, title: mock.chapterTitle(n) };
		}),
		prevChapterId: undefined,
		nextChapterId: undefined,
	};
}

export function getReaderChapter(seriesId: string, chParam?: string | null): Promise<ReaderView> {
	const canonical =
		isCanonicalId(seriesId) && !!backend.canonicalChapters && !!backend.canonicalPages;
	return live(async () => {
		const chs = canonical
			? await backend.canonicalChapters!(seriesId)
			: await backend.chapters(seriesId);
		if (!chs.length) return readerFallback(seriesId);
		// Canonical chapters already arrive server-ordered ascending with number-less/
		// oneshot rows last; re-sorting by number here would float a oneshot (wire value
		// 0) to the front, contradicting that order (CR4). Only the Suwayomi path — whose
		// backend order isn't guaranteed ascending — is sorted.
		const asc = canonical ? chs : [...chs].sort((a, b) => a.number - b.number);
		let target = chParam ? chs.find((c) => c.id === chParam) : undefined;
		if (!target) target = asc.find((c) => !c.read) ?? asc[0];
		const domainPages = canonical
			? await backend.canonicalPages!(target.id)
			: await backend.pages(target.id);
		const urls = await Promise.all(domainPages.map((p) => images.resolvePage(p)));
		const idx = asc.findIndex((c) => c.id === target.id);
		const series = await (
			canonical ? backend.canonicalSeries!(seriesId) : backend.series(seriesId)
		).catch(() => null);
		return {
			seriesId,
			seriesTitle: series?.title ?? 'Reader',
			chapterId: target.id,
			chNum: target.number,
			chTitle: target.title || `Chapter ${target.number}`,
			pages: urls.map((url, index) => ({
				url,
				index,
				ratio: '800 / 1200',
				label: String(index + 1).padStart(2, '0'),
				dim: '',
			})),
			chapters: asc
				.slice()
				.reverse()
				.map((c) => ({ id: c.id, n: c.number, title: c.title || `Chapter ${c.number}` })),
			prevChapterId: idx > 0 ? asc[idx - 1].id : undefined,
			nextChapterId: idx >= 0 && idx < asc.length - 1 ? asc[idx + 1].id : undefined,
		};
	}, readerFallback(seriesId));
}

export interface ProfileView {
	name: string;
	handle: string;
	since: string;
	bio: string;
	badge: string;
	stats: { value: string; label: string }[];
	reading: { id?: string; title: string; genre: string; ch: number; total: number }[];
	favGenres: { name: string; pct: number }[];
	activity: { icon: string; iconBg: string; text: string; time: string }[];
	shelves: {
		id?: string;
		title: string;
		genre: string;
		rating: string;
		shelf: string;
		ch: number;
		total: number;
	}[];
}

/**
 * The signed-in user's profile, built from their session identity + real reading
 * state. Signed-out (or backend off/error) falls back to the sample profile.
 *
 * NOTE: reading/library reflects the shared Suwayomi state this MVP federates,
 * and "activity" is derived from that progress — there is no per-user timestamped
 * event log yet, so a true activity stream would need a new server capability.
 */
export function getProfile(): Promise<ProfileView> {
	return live<ProfileView>(async () => {
		const session = await backend.session();
		if (!session) return mock.profile;
		const user = session.user;
		const lib = await backend.library().catch(() => [] as Series[]);
		const rows = await Promise.all(
			lib.map(async (s) => {
				const chs = await backend.chapters(s.id).catch(() => [] as Chapter[]);
				const total = chs.length || s.chapterCount;
				const read = chs.filter((c) => c.read).length;
				return { s, total, read };
			}),
		);
		const chaptersRead = rows.reduce((n, r) => n + r.read, 0);
		const readingNow = rows.filter((r) => r.read > 0 && r.read < r.total);
		const completed = rows.filter((r) => r.total > 0 && r.read >= r.total);
		const libAvg = lib.length ? lib.reduce((n, s) => n + s.rating.average, 0) / lib.length : 0;

		const freq = new Map<string, number>();
		for (const { s } of rows) for (const g of s.genres) freq.set(g, (freq.get(g) ?? 0) + 1);
		const totalG = [...freq.values()].reduce((a, b) => a + b, 0);
		const favGenres = [...freq.entries()]
			.sort((a, b) => b[1] - a[1])
			.slice(0, 5)
			.map(([name, c]) => ({ name, pct: totalG ? Math.round((c / totalG) * 100) : 0 }));

		const reading = readingNow.slice(0, 6).map(({ s, read, total }) => ({
			id: s.id,
			title: s.title,
			genre: s.genres[0] ?? '',
			ch: read,
			total,
		}));

		const shelves = rows.map(({ s, read, total }) => ({
			id: s.id,
			title: s.title,
			genre: s.genres[0] ?? '',
			rating: s.rating.average.toFixed(1),
			shelf: total > 0 && read >= total ? 'completed' : 'reading',
			ch: read,
			total,
		}));

		const activity = [
			...readingNow.slice(0, 4).map(({ s, read, total }) => ({
				icon: '📖',
				iconBg: 'rgba(255,255,255,0.07)',
				text: `Reading ${s.title} — Ch. ${read} / ${total}`,
				time: `${total ? Math.round((read / total) * 100) : 0}% complete`,
			})),
			...completed.slice(0, 2).map(({ s }) => ({
				icon: '✓',
				iconBg: 'rgba(95,191,126,0.16)',
				text: `Finished ${s.title}`,
				time: 'In your library',
			})),
		];

		return {
			name: user.username,
			handle: `@${user.username}`,
			since: user.isAdmin ? 'Administrator' : 'Reader',
			bio: lib.length
				? `${chaptersRead} chapters read across ${lib.length} series in your library.`
				: 'Your library is empty — add a series to start tracking your reading.',
			badge: user.isAdmin ? 'ADMIN' : 'READER',
			stats: [
				{ value: String(lib.length), label: 'In library' },
				{ value: String(chaptersRead), label: 'Chapters read' },
				{ value: String(readingNow.length), label: 'Reading now' },
				{ value: libAvg ? libAvg.toFixed(1) : '—', label: 'Library avg' },
			],
			reading,
			favGenres,
			activity,
			shelves,
		};
	}, mock.profile);
}

export function getDonate() {
	return Promise.resolve({
		donateTiers: mock.donateTiers,
		donateAmounts: mock.donateAmounts,
		donateAllocation: mock.donateAllocation,
	});
}

export function getSupport() {
	return Promise.resolve({ supportCategories: mock.supportCategories, faqs: mock.faqs });
}
