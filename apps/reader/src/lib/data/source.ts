/**
 * The data repository the UI consumes (via SvelteKit `load` functions).
 *
 * Each getter returns the exact view shapes the screens render. When the backend
 * is enabled (`PUBLIC_KOMIKA_BACKEND=on`) it maps live `@komika/api` domain data
 * into those shapes; otherwise — or if a live call throws — it falls back to
 * `mock.ts`, so the app is always renderable. This is the single seam to swap as
 * the backend fills in: screens never import `mock.ts` directly.
 */
import type { Chapter, ComicType as DomainComicType, Series, SeriesStatus } from '@komika/types';
import { backend, images } from '$lib/context';
import { config } from '$lib/config';
import * as mock from './mock';
import type { Card, CatalogEntry, ComicType, Status } from './mock';

const LIVE = config.backendEnabled;

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

// ---- per-screen getters ----------------------------------------------------

export function getHome() {
	const fallback = {
		featured: mock.featured,
		latestUpdates: mock.latestUpdates,
		trending: mock.trending,
		latestAdded: mock.latestAdded,
		formatCards: mock.formatCards,
		homeGenres: mock.homeGenres,
	};
	return live(async () => {
		const feeds = await backend.discovery();
		const byKind = (k: string) => feeds.find((f) => f.kind === k)?.items ?? [];
		const featured = (byKind('POPULAR').length ? byKind('POPULAR') : (feeds[0]?.items ?? []))
			.slice(0, 5)
			.map((s) => ({ title: s.title, genre: s.genres[0] ?? '', ch: s.chapterCount }));
		return {
			featured: featured.length ? featured : mock.featured,
			latestUpdates: byKind('RECENTLY_UPDATED').map(toCard),
			trending: byKind('TRENDING').map(toCard),
			latestAdded: byKind('RECENTLY_ADDED').map(toCard),
			formatCards: mock.formatCards,
			homeGenres: mock.homeGenres,
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
		const feeds = await backend.discovery();
		const byKind = (k: string) => feeds.find((f) => f.kind === k)?.items ?? [];
		const recent = byKind('RECENTLY_UPDATED').map(toCard);
		const trending = byKind('TRENDING').map(toCard);
		return {
			trendingGroups: trending.length
				? [{ label: 'Trending Today', items: trending }]
				: mock.trendingGroups,
			newUpdates: recent.length ? recent : mock.newUpdates,
			hotUpdates: trending.length ? trending : mock.hotUpdates,
		};
	}, fallback);
}

export function getLibrary() {
	const fallback = { libraryCatalog: mock.libraryCatalog, continueRow: mock.continueRow };
	return live(async () => {
		const lib = await backend.library();
		if (!lib.length) return fallback;
		const libraryCatalog = lib.map((s) => ({
			title: s.title,
			genre: s.genres[0] ?? '',
			rating: s.rating.average.toFixed(1),
			shelf: 'reading' as const,
			read: 0,
			total: s.chapterCount,
		}));
		return { libraryCatalog, continueRow: mock.continueRow };
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
	followers: string;
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
			followers: mock.seriesDetail.followers,
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

function mapSeriesView(s: Series, chs: Chapter[]): SeriesView {
	const type = toViewType(s.type);
	const asc = [...chs].sort((a, b) => a.number - b.number);
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
			followers: '—',
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
		related: [],
	};
}

export function getSeries(id: string): Promise<SeriesView> {
	return live(async () => {
		const [s, chs] = await Promise.all([backend.series(id), backend.chapters(id)]);
		return mapSeriesView(s, chs);
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
	return live(async () => {
		const chs = await backend.chapters(seriesId);
		if (!chs.length) return readerFallback(seriesId);
		const asc = [...chs].sort((a, b) => a.number - b.number);
		let target = chParam ? chs.find((c) => c.id === chParam) : undefined;
		if (!target) target = asc.find((c) => !c.read) ?? asc[0];
		const domainPages = await backend.pages(target.id);
		const urls = await Promise.all(domainPages.map((p) => images.resolvePage(p)));
		const idx = asc.findIndex((c) => c.id === target.id);
		const series = await backend.series(seriesId).catch(() => null);
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

export function getProfile() {
	// Profile is user-specific; wire to session + activity once auth lands.
	return Promise.resolve(mock.profile);
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
