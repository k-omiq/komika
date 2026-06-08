/**
 * The data repository the UI consumes (via SvelteKit `load` functions).
 *
 * Each getter returns the exact view shapes the screens render. When the backend
 * is enabled (`PUBLIC_KOMIKA_BACKEND=on`) it maps live `@komika/api` domain data
 * into those shapes. Getters never reject: when the backend is off, errors, or
 * returns nothing, they resolve to honest empty results (empty arrays / null) and
 * the screens render their empty states. No sample content is ever fabricated.
 */
import type {
	AggregatedChapter,
	CanonicalUpdate,
	Chapter,
	ComicType as DomainComicType,
	DiscoveryFeed,
	Series,
	SeriesProgress,
	SeriesStatus,
	Translator,
	WorkSource,
} from '@komika/types';
import { backend, images } from '$lib/context';
import { getPreferredTranslator } from './translator-pref.svelte';
import { config } from '$lib/config';
import * as content from './content';
import { FLAG, FORMAT_CARDS } from './types';
import type { Card, CatalogEntry, ComicType, Shelf, Status } from './types';

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

/** Run a live mapping, resolving to the (empty) fallback on any failure. */
async function live<T>(fn: () => Promise<T>, fallback: T): Promise<T> {
	if (!LIVE) return fallback;
	try {
		return await fn();
	} catch (err) {
		console.warn('[komika] backend call failed:', err);
		return fallback;
	}
}

/**
 * Like {@link live} but tells a genuine backend FAILURE apart from an honest
 * empty result: returns `{ data, error }` where `error` is true only when the
 * mapping threw. Screens use this to show an error state (during an outage)
 * instead of a misleading "not found" / "no results" empty state. Mock mode
 * (`!LIVE`) is never an error — it's just empty.
 */
async function liveResult<T>(fn: () => Promise<T>, empty: T): Promise<{ data: T; error: boolean }> {
	if (!LIVE) return { data: empty, error: false };
	try {
		return { data: await fn(), error: false };
	} catch (err) {
		console.warn('[komika] backend call failed:', err);
		return { data: empty, error: true };
	}
}

/** Coarse "N ago" relative time for the activity feed. */
function relTimeAgo(iso: string | null | undefined): string {
	if (!iso) return 'just now';
	const then = Date.parse(iso);
	if (Number.isNaN(then)) return 'just now';
	const now = typeof Date !== 'undefined' && Date.now ? Date.now() : then;
	const mins = Math.max(0, Math.round((now - then) / 60000));
	if (mins < 1) return 'just now';
	if (mins < 60) return `${mins}m ago`;
	const hrs = Math.round(mins / 60);
	if (hrs < 24) return `${hrs}h ago`;
	const days = Math.round(hrs / 24);
	if (days < 30) return `${days}d ago`;
	return monthYear(iso);
}

/** "March 2023"-style month/year for the profile "joined" line. */
function monthYear(iso: string | null | undefined): string {
	if (!iso) return 'recently';
	const d = new Date(iso);
	if (Number.isNaN(d.getTime())) return 'recently';
	return d.toLocaleDateString(undefined, { month: 'long', year: 'numeric' });
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
		type: toViewType(s.type),
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
	// `added` stays the positional (backend-order) index for back-compat; `addedAt`
	// carries the real catalogue-entry timestamp so Browse "Newest" can sort by
	// recency (b.addedAt - a.addedAt) rather than by arrival order. NaN/absent → 0.
	const addedAt = s.createdAt ? Date.parse(s.createdAt) : NaN;
	return {
		title: s.title,
		author: s.author ?? '',
		genre: s.genres[0] ?? '',
		ch: s.chapterCount,
		rating: s.rating.average,
		status: toViewStatus(s.status),
		added: i,
		addedAt: Number.isFinite(addedAt) ? addedAt : 0,
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
 * feeds), keeping the cards' presentation metadata. Not the true global total —
 * the federated catalog isn't fully enumerated client-side — but a real
 * reflection of what's currently surfaced.
 */
function deriveFormatCards(pool: Series[]): typeof FORMAT_CARDS {
	return FORMAT_CARDS.map((card) => {
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

// ---- translators (per-source "translator" selection, S3) -------------------

/** A selectable translator (source) for a canonical work, in view shape. */
export interface TranslatorOption {
	/** Stable selection key (`sourceType:suwayomiMangaId`), persisted per work. */
	key: string;
	/** `'mangadex'` (canonical spine) or `'suwayomi'` (an installed extension). */
	sourceType: string;
	/** Display name, e.g. "MangaDex" or "MANGA Plus". */
	name: string;
	/** Language code (e.g. `en`), or null when N/A. */
	lang: string | null;
	/** Store-hosted extension logo, or null → the UI renders an initial. */
	iconUrl: string | null;
	/** Suwayomi manga id whose `chapters(seriesId:)` gives this translator's
	 *  chapters; null → the canonical spine (`canonicalChapters(workId)`). */
	suwayomiMangaId: string | null;
	/** How many chapters this translator currently carries. */
	chapterCount: number;
}

/** A compact translator tag for cards (logo + label). */
export interface TranslatorChip {
	name: string;
	lang: string | null;
	iconUrl: string | null;
}

/** Friendly display names for the common curated extensions; falls back to a
 *  prettified last package segment (e.g. `…extension.all.foo` → "Foo"). */
const SOURCE_NAME_BY_PKG: Record<string, string> = {
	'eu.kanade.tachiyomi.extension.all.mangadex': 'MangaDex',
	'eu.kanade.tachiyomi.extension.all.mangaplus': 'MANGA Plus',
	'eu.kanade.tachiyomi.extension.all.comick': 'ComicK',
	'eu.kanade.tachiyomi.extension.en.webtoons': 'WEBTOON',
	'eu.kanade.tachiyomi.extension.all.batoto': 'Bato.to',
};

function prettySourceName(pkg: string | null | undefined): string | null {
	if (!pkg) return null;
	if (SOURCE_NAME_BY_PKG[pkg]) return SOURCE_NAME_BY_PKG[pkg];
	const seg = pkg.split('.').pop() ?? pkg;
	return seg.charAt(0).toUpperCase() + seg.slice(1);
}

/** The store-hosted icon URL Keiyoushi publishes for an extension package. */
function iconForPkg(pkg: string | null | undefined): string | null {
	return pkg ? `https://raw.githubusercontent.com/keiyoushi/extensions/repo/icon/${pkg}.png` : null;
}

/** A normalized language label, dropping Suwayomi's catch-all "all". */
function langLabel(lang: string | null | undefined): string | null {
	return lang && lang !== 'all' ? lang : null;
}

function translatorKey(sourceType: string, suwayomiMangaId: string | null): string {
	return `${sourceType}:${suwayomiMangaId ?? 'spine'}`;
}

/** Federated-search `Translator` → compact card chip. */
function toTranslatorChip(t: Translator): TranslatorChip {
	return {
		name: t.sourceName ?? prettySourceName(t.extensionPkgName) ?? 'Source',
		lang: langLabel(t.lang),
		iconUrl: t.extensionIconUrl ?? iconForPkg(t.extensionPkgName),
	};
}

/** Dedupe chips by name+lang, preserving first-seen order (a work can carry the
 *  same source under several manga ids — collapse them for display). */
function dedupeChips(list: TranslatorChip[]): TranslatorChip[] {
	const seen = new Set<string>();
	const out: TranslatorChip[] = [];
	for (const c of list) {
		const k = `${c.name}|${c.lang ?? ''}`;
		if (!seen.has(k)) {
			seen.add(k);
			out.push(c);
		}
	}
	return out;
}

/** A resolved canonical work: its selectable translators, the chosen one, and
 *  that translator's chapters + series metadata. Null when nothing readable. */
interface ResolvedWork {
	translators: TranslatorOption[];
	selected: TranslatorOption;
	chapters: Chapter[];
	meta: Series | null;
	/** The canonical (MangaDex-mirrored) series for the work when it resolves —
	 *  carries S2 enrichment (credits + localized descriptions). Null for
	 *  federation-only works with no MangaDex anchor. */
	canonSeries: Series | null;
	/** Whether `chapters` are already server-ordered (the canonical spine). */
	preserveOrder: boolean;
}

/**
 * Resolve a canonical (`w_`) work into its translator list + the selected
 * translator's chapters/metadata. Fans out over the work's source mappings
 * (`workSources`) plus the MangaDex canonical spine when present, fetching each
 * translator's chapters so the picker can show counts and a non-empty default is
 * chosen. The SAME resolver backs both the series page and the reader, so they
 * always agree on the default translator. Returns null when the work has no
 * readable source at all.
 */
async function resolveWork(workId: string, preferredKey?: string): Promise<ResolvedWork | null> {
	// Source mappings (suwayomi translators) + the canonical spine chapters + the
	// canonical series (S2 enrichment: credits + localized descriptions), in
	// parallel. Individual rejections are tolerated (canonicalChapters/canonicalSeries
	// legitimately 404 for a federation-only work), but if every method that is
	// actually present rejects the backend is effectively down — rethrow so the caller
	// surfaces an honest error rather than a false "not found".
	const hasWorkSources = !!backend.workSources;
	const hasCanonChapters = !!backend.canonicalChapters;
	const hasCanonSeries = !!backend.canonicalSeries;
	const [wsRes, spineRes, canonRes] = await Promise.allSettled([
		hasWorkSources ? backend.workSources!(workId) : Promise.resolve([] as WorkSource[]),
		hasCanonChapters ? backend.canonicalChapters!(workId) : Promise.resolve([] as Chapter[]),
		hasCanonSeries ? backend.canonicalSeries!(workId) : Promise.resolve(null),
	]);
	// "Backend fully down" honest-error guard: an ABSENT optional method resolves to a
	// benign default and must NOT be counted as evidence the backend is up (otherwise a
	// backend missing e.g. canonicalChapters would mask a real outage of the methods it
	// DOES have and degrade to a false "not found"). Rethrow only when every method that
	// was actually present rejected — a true outage — and at least one was present.
	const present = [
		{ has: hasWorkSources, res: wsRes },
		{ has: hasCanonChapters, res: spineRes },
		{ has: hasCanonSeries, res: canonRes },
	].filter((m) => m.has);
	if (present.length > 0 && present.every((m) => m.res.status === 'rejected')) {
		throw (present.find((m) => m.res.status === 'rejected')!.res as PromiseRejectedResult).reason;
	}
	const wsList: WorkSource[] = wsRes.status === 'fulfilled' ? wsRes.value : [];
	const spineChapters: Chapter[] = spineRes.status === 'fulfilled' ? spineRes.value : [];
	const canonSeries: Series | null = canonRes.status === 'fulfilled' ? canonRes.value : null;

	// Candidate translators: canonical spine first (when the mirror has chapters),
	// then each distinct suwayomi source mapping.
	const candidates: { view: Omit<TranslatorOption, 'chapterCount'>; chapters: Chapter[] }[] = [];
	if (spineChapters.length) {
		candidates.push({
			view: {
				key: translatorKey('mangadex', null),
				sourceType: 'mangadex',
				name: 'MangaDex',
				lang: null,
				iconUrl: iconForPkg('eu.kanade.tachiyomi.extension.all.mangadex'),
				suwayomiMangaId: null,
			},
			chapters: spineChapters,
		});
	}

	const seenIds = new Set<string>();
	const suwViews: Omit<TranslatorOption, 'chapterCount'>[] = [];
	for (const ws of wsList) {
		if (ws.sourceType === 'mangadex') continue; // spine handled above
		const id = ws.sourceKey;
		if (!id || seenIds.has(id)) continue;
		seenIds.add(id);
		suwViews.push({
			key: translatorKey(ws.sourceType, id),
			sourceType: ws.sourceType,
			name: prettySourceName(ws.extension?.pkgName) ?? `Source ${ws.sourceId}`,
			lang: langLabel(ws.lang),
			iconUrl: iconForPkg(ws.extension?.pkgName),
			suwayomiMangaId: id,
		});
	}
	// Fetch each suwayomi translator's chapters (bounded — a work has few mappings).
	const suwChapters = await Promise.all(
		suwViews.map((v) => backend.chapters(v.suwayomiMangaId as string).catch(() => [] as Chapter[])),
	);
	suwViews.forEach((v, i) => candidates.push({ view: v, chapters: suwChapters[i] }));

	if (!candidates.length) return null;

	// Order: spine first, then by chapter count desc so a populated translator
	// leads. Stable within equal counts (preserves discovery order).
	const ordered = candidates
		.map((c, i) => ({ ...c, i }))
		.sort((a, b) => {
			const aSpine = a.view.suwayomiMangaId === null ? 1 : 0;
			const bSpine = b.view.suwayomiMangaId === null ? 1 : 0;
			if (aSpine !== bSpine) return bSpine - aSpine;
			if (b.chapters.length !== a.chapters.length) return b.chapters.length - a.chapters.length;
			return a.i - b.i;
		});

	const translators: TranslatorOption[] = ordered.map((c) => ({
		...c.view,
		chapterCount: c.chapters.length,
	}));

	// Selection: honour a valid persisted preference, else the candidate carrying
	// the MOST chapters (so a work whose spine has few/none but another source is
	// complete — e.g. Solo Leveling → Asura — defaults to the readable source),
	// else the first candidate.
	const byMostChapters = [...ordered].sort((a, b) => b.chapters.length - a.chapters.length)[0];
	let pick =
		(preferredKey && ordered.find((c) => c.view.key === preferredKey)) ||
		byMostChapters ||
		ordered[0];

	const selected = translators.find((t) => t.key === pick.view.key) as TranslatorOption;

	// Metadata for the selected translator: the canonical mirror for the spine
	// (already fetched), otherwise the source series behind the chosen suwayomi
	// mapping.
	let meta: Series | null;
	if (selected.suwayomiMangaId === null) {
		meta = canonSeries;
	} else {
		meta = await backend.series(selected.suwayomiMangaId).catch(() => null);
	}

	return {
		translators,
		selected,
		chapters: pick.chapters,
		meta,
		canonSeries,
		preserveOrder: selected.suwayomiMangaId === null,
	};
}

/**
 * The outcome of a catalogue search. The federated (`searchAllSources`) path is
 * login-gated and per-user rate-limited on the server, so callers need to tell
 * "no matches" apart from "not authenticated" (fall back to native) and
 * "rate-limited" (honest transient message) rather than collapsing every case to
 * an empty grid.
 */
export type SearchOutcome =
	| { kind: 'ok'; rows: FederatedResultView[] }
	| { kind: 'unauthenticated' }
	| { kind: 'rateLimited'; retryAfter: number | null }
	| { kind: 'error' };

/**
 * Federated multi-extension search: one deduped row per canonical work, each
 * carrying its per-source translator tags. REQUIRES a signed-in viewer (the
 * server rejects anonymous callers) — callers should only invoke it when logged
 * in and fall back to {@link getNativeSearch} otherwise. Never rejects; classifies
 * the server's auth/rate-limit errors into {@link SearchOutcome}.
 */
export async function getFederatedSearch(query: string): Promise<SearchOutcome> {
	const q = query.trim();
	if (!q) return { kind: 'ok', rows: [] };
	if (!LIVE || !backend.searchAllSources) {
		// Backend without federation — treat as unauthenticated so callers use the
		// public native path (which carries no translators).
		return { kind: 'unauthenticated' };
	}
	try {
		const page = await backend.searchAllSources(q);
		return {
			kind: 'ok',
			rows: page.items.map((fs, i) => ({
				...toCatalogEntryFull(fs.series, i),
				translators: dedupeChips(fs.translators.map(toTranslatorChip)),
			})),
		};
	} catch (err) {
		const msg = err instanceof Error ? err.message : String(err);
		if (/not authenticated/i.test(msg)) return { kind: 'unauthenticated' };
		if (/too many/i.test(msg)) {
			const m = msg.match(/(\d+)\s*s/i);
			return { kind: 'rateLimited', retryAfter: m ? Number(m[1]) : null };
		}
		console.warn('[komika] federated search failed:', err);
		return { kind: 'error' };
	}
}

/** Browse-search filters applied server-side where supported (S4). */
export interface CatalogFilters {
	/** Match any of these genres (case-insensitive). Empty → no genre filter. */
	genres?: string[];
	/** Inclusive rating bounds on the 0–10 scale. */
	minRating?: number;
	maxRating?: number;
}

/**
 * Native (public) catalogue search — the pre-federation path, available to
 * anonymous viewers, and the whole-catalogue browse when `query` is empty. Genre
 * / rating `filters` are applied server-side (S4). Rows carry no translator tags
 * (single source). Never rejects; `error` is true only on a genuine backend
 * failure (an empty query with no error is an honest "no results").
 */
export async function getNativeSearch(
	query: string,
	filters: CatalogFilters = {},
): Promise<{ items: FederatedResultView[]; error: boolean }> {
	const r = await liveResult(async () => {
		const { items } = await backend.search(query.trim(), 1, {
			genres: filters.genres,
			minRating: filters.minRating,
			maxRating: filters.maxRating,
		});
		return items.map((s, i) => ({
			...toCatalogEntryFull(s, i),
			translators: [] as TranslatorChip[],
		}));
	}, [] as FederatedResultView[]);
	return { items: r.data, error: r.error };
}

/** The full genre facet set for the search filter (S4), most-common first. Never
 *  rejects; resolves to an empty list on failure/backend-off. */
export function getGenreFacets(): Promise<{ genre: string; count: number }[]> {
	return live(async () => {
		if (!backend.genreFacets) return [];
		return backend.genreFacets();
	}, []);
}

/** A federated search row for the Browse grid: a catalog entry + translator tags
 *  + the full genre list (for client-side ANY-genre filtering of federated rows). */
export interface FederatedResultView extends CatalogEntry {
	isNsfw: boolean;
	genres: string[];
	translators: TranslatorChip[];
}

/** Like {@link toCatalogEntry} but keeps the NSFW flag + full genre list for the grid. */
function toCatalogEntryFull(
	s: Series,
	i: number,
): CatalogEntry & { isNsfw: boolean; genres: string[] } {
	return { ...toCatalogEntry(s, i), isNsfw: s.isNsfw, genres: s.genres };
}

// ---- per-screen getters ----------------------------------------------------

export function getHome() {
	const fallback = {
		featured: [] as FeaturedView[],
		latestUpdates: [] as Card[],
		trending: [] as Card[],
		latestAdded: [] as Card[],
		formatCards: FORMAT_CARDS,
		homeGenres: [] as string[],
	};
	return live(async () => {
		// Discovery drives the curated rows; the scanner-backed `updates` feed drives
		// "Latest Updates" (not Suwayomi's source "Latest"). The hero is the top of
		// the live Popular feed (first feed as fallback) — empty when nothing is live.
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
			featured,
			latestUpdates: updates.items.map(toCard),
			trending: byKind('TRENDING').map(toCard),
			latestAdded: byKind('RECENTLY_ADDED').map(toCard),
			formatCards: pool.length ? deriveFormatCards(pool) : FORMAT_CARDS,
			homeGenres: deriveGenres(pool),
		};
	}, fallback);
}

/** The browse catalogue rows (with genres/translator shape) plus an honest
 *  failure flag (RC4). `error` is true only when the backend request threw — the
 *  screen shows an error state instead of a misleading "no series" empty. Genre /
 *  rating `filters` are applied server-side (S4). */
export interface CatalogResult {
	items: FederatedResultView[];
	error: boolean;
}

export function getBrowseCatalog(filters: CatalogFilters = {}): Promise<CatalogResult> {
	return getNativeSearch('', filters);
}

export function getUpdates() {
	const fallback = {
		trendingGroups: [] as { label: string; items: Card[] }[],
		newUpdates: [] as Card[],
		hotUpdates: [] as Card[],
	};
	return live(async () => {
		// "New" is the scanner-driven Updates feed (series with freshly-detected
		// chapters, newest-first). "Trending"/"Hot" reuse the discovery Trending
		// feed. Empty (no detections yet) renders the page's empty state.
		// Each feed is caught independently (mirrors getHome): one outage — e.g. the
		// scanner `updates` feed — must not collapse the whole screen (incl. Trending).
		const [feeds, updates, canonical] = await Promise.all([
			backend.discovery().catch(() => [] as DiscoveryFeed[]),
			backend.updates().catch(() => ({ items: [] as Series[] })),
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

export interface LibraryRowView {
	id?: string;
	title: string;
	cover: string;
	genre: string;
	rating: string;
	shelf: Shelf;
	favorite: boolean;
	read: number;
	total: number;
}
export interface ContinueRowView {
	id?: string;
	title: string;
	cover: string;
	ch: string;
	progress: number;
	genre: string;
}

export function getLibrary() {
	const fallback = {
		libraryCatalog: [] as LibraryRowView[],
		continueRow: [] as ContinueRowView[],
	};
	return live<typeof fallback>(async () => {
		const lib = await backend.library();
		// Per-series read progress in ONE batched query, joined by id. This replaces
		// a `chapters()` fetch per series — an N-round-trip fan-out that hung this
		// page on a large library (hundreds of series). Series without cached
		// progress fall back to their chapter count as unread. Backends that don't
		// expose `libraryProgress` (Suwayomi/native) degrade to all-unread.
		const progress = backend.libraryProgress
			? await backend.libraryProgress().catch(() => [] as SeriesProgress[])
			: [];
		const byId = new Map(progress.map((p) => [p.id, p]));
		const rows = lib.map((s) => {
			const p = byId.get(s.id);
			const total = p?.total || s.chapterCount;
			const read = p?.read ?? 0;
			return { s, total, read };
		});
		const libraryCatalog = rows.map(({ s, total, read }) => ({
			id: s.id,
			title: s.title,
			cover: s.coverUrl,
			genre: s.genres[0] ?? '',
			rating: s.rating.average.toFixed(1),
			// An explicit shelf the viewer filed wins; otherwise derive from progress.
			shelf: (s.libraryStatus as Shelf | null) ?? shelfFor(read, total),
			favorite: s.isFavorite ?? false,
			read,
			total,
		}));
		// Continue-reading: series with progress underway. The resume label is the
		// next chapter number (read + 1), since chapters run 1..total.
		const continueRow = rows
			.filter(({ read, total }) => read > 0 && read < total)
			.map(({ s, read, total }) => ({
				id: s.id,
				title: s.title,
				cover: s.coverUrl,
				ch: `Ch. ${read + 1}`,
				progress: total ? Math.round((read / total) * 100) : 0,
				genre: s.genres[0] ?? '',
			}));
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
	/** The Suwayomi manga id of the source that provides this chapter (S2
	 *  aggregation), so the reader opens it from the right source; null = the
	 *  MangaDex mirror; undefined for a single-source (numeric) series. */
	src?: string | null;
}
export interface SeriesDetailView {
	id: string;
	title: string;
	type: ComicType;
	flag: string;
	rating: string;
	votes: string;
	totalCh: number;
	/** All-time view count (chapter opens) — the popularity stat shown on the page.
	 *  0 until reads accrue or when the backend doesn't track views. */
	viewsTotal: number;
	updated: string;
	statusLabel: string;
	author: string;
	artist: string;
	genres: string[];
	synopsis: string;
	cover: string;
	continueCh: number;
	startChapterId?: string;
	/** The source (Suwayomi manga id) the Continue/start chapter is read from
	 *  (S2 aggregation); null = MangaDex mirror; undefined for single-source. */
	startChapterSrc?: string | null;
	isMarked: boolean;
	/** Whether the viewer favourited this series (per-viewer; false when signed out). */
	isFavorite: boolean;
	/** The viewer's explicit library shelf, or null to derive from progress. */
	libraryStatus: Shelf | null;
	/** Full author/artist credit list (S2 enrichment); empty → fall back to the
	 *  single author/artist line. Deduped display is left to the component. */
	credits: { role: string; name: string }[];
	/** Every localized description of the work (S2); empty when un-enriched. Lets
	 *  the page offer other languages beyond the default-picked `synopsis`. */
	descriptions: { lang: string; description: string }[];
	/** The language tag of the description shown in `synopsis` (or null). */
	descLang: string | null;
	/** The full MangaDex cover set (F2), primary first then volume-ordered; empty
	 *  for a non-canonical / un-enriched work. The gallery renders only when >1. */
	covers: CoverView[];
}

/** One cover in the series-page gallery (F2). URLs are proxy-ready — render via
 *  the {@link Cover} component / ImageProvider, not as a raw <img src>. */
export interface CoverView {
	thumbnailUrl: string;
	url: string;
	lang: string | null;
	volume: string | null;
	isPrimary: boolean;
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
	/** Available translators (sources) for this work; empty for single-source
	 *  (numeric Suwayomi) series that have no per-source alternatives. */
	translators: TranslatorOption[];
	/** The selected translator's key (matches one of {@link translators}). */
	selectedTranslatorKey: string | null;
	/** The canonical `w_` workId to switch translators against; null when N/A. */
	workId: string | null;
}

const STATUS_WORD: Record<SeriesStatus, string> = {
	ONGOING: 'ONGOING',
	COMPLETED: 'COMPLETED',
	HIATUS: 'HIATUS',
	CANCELLED: 'CANCELLED',
	UNKNOWN: 'ONGOING',
};

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

interface TranslatorMeta {
	translators: TranslatorOption[];
	selectedTranslatorKey: string | null;
	workId: string | null;
}

const NO_TRANSLATORS: TranslatorMeta = {
	translators: [],
	selectedTranslatorKey: null,
	workId: null,
};

/** The viewer's/app language tag (lowercased), e.g. `en`, `pt-br`. */
function appLang(): string {
	if (typeof navigator !== 'undefined' && navigator.language)
		return navigator.language.toLowerCase();
	return 'en';
}

/**
 * Pick the localized description best matching the app language from the work's
 * enrichment: exact tag → base language (`pt` for `pt-br`) → any English → the
 * work's default description. Returns the chosen text + its language tag.
 */
function pickLocalizedDescription(
	enrich: Series | null,
	fallback: string,
): { text: string; lang: string | null } {
	const list = enrich?.localizedDescriptions ?? [];
	if (!list.length) return { text: fallback, lang: null };
	const want = appLang();
	const base = want.split('-')[0];
	const exact = list.find((d) => d.lang.toLowerCase() === want);
	const baseMatch = list.find((d) => d.lang.toLowerCase().split('-')[0] === base);
	const en = list.find((d) => d.lang.toLowerCase().split('-')[0] === 'en');
	const chosen = exact ?? baseMatch ?? en ?? null;
	return chosen ? { text: chosen.description, lang: chosen.lang } : { text: fallback, lang: null };
}

function mapSeriesView(
	s: Series,
	chs: Chapter[],
	pool: Series[],
	preserveOrder = false,
	tmeta: TranslatorMeta = NO_TRANSLATORS,
	enrich: Series | null = null,
): SeriesView {
	const type = toViewType(s.type);
	const desc = pickLocalizedDescription(enrich, s.description ?? '');
	const credits = enrich?.credits ?? [];
	const descriptions = enrich?.localizedDescriptions ?? [];
	const covers: CoverView[] = (enrich?.covers ?? []).map((c) => ({
		thumbnailUrl: c.thumbnailUrl,
		url: c.url,
		lang: c.lang,
		volume: c.volume,
		isPrimary: c.isPrimary,
	}));
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
			flag: FLAG[type],
			rating: s.rating.average.toFixed(1),
			votes: String(s.rating.count),
			totalCh: s.chapterCount || chs.length,
			viewsTotal: s.views?.total ?? 0,
			updated: relTime(s.updatedAt) || 'recently',
			statusLabel: STATUS_WORD[s.status],
			author: s.author ?? '',
			artist: s.artist ?? '',
			genres: s.genres,
			synopsis: desc.text,
			cover: s.coverUrl,
			continueCh: firstUnread?.number ?? 1,
			startChapterId: firstUnread?.id,
			isMarked: s.isMarked,
			isFavorite: s.isFavorite ?? false,
			libraryStatus: (s.libraryStatus as Shelf | null) ?? null,
			credits,
			descriptions,
			descLang: desc.lang,
			covers,
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
		translators: tmeta.translators,
		selectedTranslatorKey: tmeta.selectedTranslatorKey,
		workId: tmeta.workId,
	};
}

/**
 * Build the series-page chapter list from the server's multi-source aggregation
 * (S2): one row per chapter NUMBER across ALL a work's sources. Each row picks
 * the source to read from — the work's preferred translator when it carries that
 * chapter, otherwise the first source that does (per-chapter fallback, so a
 * chapter the preferred source lacks is still readable). Read-state + dates are
 * enriched from the preferred translator's own chapters where they line up.
 */
function buildAggregatedChapters(
	agg: AggregatedChapter[],
	resolved: ResolvedWork,
): SeriesChapterView[] {
	const selectedSuw = resolved.selected.suwayomiMangaId ?? null;
	// The selected translator's OWN chapters are guaranteed readable (they came from
	// `chapters(selectedSuw)` / the mirror). Index them by number so an aggregated
	// chapter the translator also carries is read from IT (its real id + date +
	// read-state), sidestepping any drift between the aggregation and a source's
	// live `chapters()`.
	const readable = new Map(resolved.chapters.map((c) => [c.number, c]));
	// Fallback source for numbers the selected translator lacks: any Suwayomi source
	// (readable via `chapters()`), else the MangaDex mirror.
	const pickSource = (sources: AggregatedChapter['sources']) =>
		sources.find((s) => s.suwayomiMangaId) ?? sources[0];
	const asc = [...agg].sort((a, b) => a.number - b.number);
	return asc.map((c, i) => {
		const own = readable.get(c.number);
		let id: string | undefined;
		let src: string | null;
		let read = false;
		let date = '';
		if (own) {
			id = own.id;
			src = selectedSuw;
			read = own.read;
			date = relTime(own.uploadedAt);
		} else {
			const chosen = pickSource(c.sources);
			id = chosen?.chapterId;
			src = chosen?.suwayomiMangaId ?? null;
		}
		return {
			id,
			n: c.number,
			title: c.title || `Chapter ${c.number}`,
			date,
			// "New" = the highest-numbered chapters (shown first when sorted newest).
			isNew: i >= asc.length - 3,
			read,
			src,
		};
	});
}

/** A series-detail view plus an honest failure flag (RC4). `view` is null for a
 *  genuinely missing series; `error` is true only when the backend request threw
 *  (outage) — the page then shows an error state, not "series not found". */
export interface SeriesResult {
	view: SeriesView | null;
	error: boolean;
}

export async function getSeries(id: string): Promise<SeriesResult> {
	const r = await liveResult<SeriesView | null>(async () => {
		// Canonical (`w_`) works: resolve the selected translator (persisted
		// preference or a sensible default) and render ITS chapters + metadata. This
		// covers both federation-only works (no MangaDex mirror) and true mirrors
		// (the canonical spine appears as one translator). See {@link resolveWork}.
		if (isCanonicalId(id)) {
			const preferred = getPreferredTranslator(id);
			const resolved = await resolveWork(id, preferred);
			if (!resolved || !resolved.meta) return null;
			// The detail id must stay the canonical `w_` workId (even though metadata
			// comes from the selected translator's source series), so chapter links,
			// the reader route and library marking all route through the canonical
			// path — where the persisted translator preference is resolved again.
			const metaForView: Series = { ...resolved.meta, id };
			const view = mapSeriesView(
				metaForView,
				resolved.chapters,
				[],
				resolved.preserveOrder,
				{
					translators: resolved.translators,
					selectedTranslatorKey: resolved.selected.key,
					workId: id,
				},
				resolved.canonSeries,
			);
			// S2: render the UNION of chapters across every source (fixes works whose
			// spine has 0 chapters but another source has them, e.g. Solo Leveling →
			// Asura's 201). Falls back to the single-translator list when aggregation
			// is unavailable/empty.
			const agg = backend.aggregatedChapters
				? await backend.aggregatedChapters(id).catch(() => [] as AggregatedChapter[])
				: [];
			if (agg.length) {
				const rows = buildAggregatedChapters(agg, resolved);
				view.chapters = rows;
				view.detail.totalCh = rows.length;
				const firstUnread = rows.find((c) => !c.read) ?? rows[0];
				view.detail.continueCh = firstUnread?.n ?? 1;
				view.detail.startChapterId = firstUnread?.id;
				view.detail.startChapterSrc = firstUnread?.src ?? null;
			}
			return view;
		}
		// A numeric Suwayomi series is a single source — no translator picker. Fetch a
		// candidate pool (Popular) alongside it to seed related-by-genre. A pool
		// failure just yields no related — it never fails the page.
		const [s, chs, pool] = await Promise.all([
			backend.series(id),
			backend.chapters(id),
			backend
				.search('')
				.then((r) => r.items)
				.catch(() => [] as Series[]),
		]);
		return mapSeriesView(s, chs, pool);
	}, null);
	return { view: r.data, error: r.error };
}

/**
 * Add/remove a series from the library. Returns the resulting marked state
 * (the backend's `isMarked`), or the optimistic value when offline/mock so the
 * UI toggle still responds.
 */
export async function setLibraryMark(seriesId: string, marked: boolean): Promise<boolean> {
	if (!LIVE) return marked;
	// Both numeric Suwayomi ids and `w_` canonical ids persist: the server `mark`
	// resolver routes on id shape (canonical → `canonical_library`) (CR6), so no
	// client-side id-shape guard is needed here — do NOT early-return the optimistic
	// value for `w_` ids, that would short-circuit canonical library persistence.
	// The try/catch below is only the defensive fallback for offline/mock.
	try {
		const s = await backend.mark(seriesId, marked);
		return s.isMarked;
	} catch (err) {
		console.warn('[komika] mark failed:', err);
		return marked;
	}
}

/**
 * File a series under an explicit shelf (`reading`/`completed`/`onhold`/`plan`), or
 * pass `null` to clear it back to progress-derived shelving. Adds the series to the
 * library if it isn't there yet. Returns the effective shelf the caller should show
 * (`null` when cleared → the UI falls back to its derived shelf), or the optimistic
 * value when offline/mock. No-op backends (Suwayomi/native) resolve optimistically.
 */
export async function setLibraryStatus(
	seriesId: string,
	status: Shelf | null,
): Promise<Shelf | null> {
	if (!LIVE || !backend.setLibraryStatus) return status;
	try {
		const s = await backend.setLibraryStatus(seriesId, status);
		return (s.libraryStatus as Shelf | null) ?? null;
	} catch (err) {
		console.warn('[komika] setLibraryStatus failed:', err);
		return status;
	}
}

/**
 * Toggle the viewer's favourite flag on a series (adds it to the library if absent).
 * Returns the resulting favourite state, or the optimistic value when offline/mock.
 */
export async function setFavorite(seriesId: string, favorite: boolean): Promise<boolean> {
	if (!LIVE || !backend.setFavorite) return favorite;
	try {
		const s = await backend.setFavorite(seriesId, favorite);
		return s.isFavorite ?? favorite;
	} catch (err) {
		console.warn('[komika] setFavorite failed:', err);
		return favorite;
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

/**
 * Record one view (a chapter open) for a series — the popularity signal behind
 * Trending and the series-page view count. Fire-and-forget: no auth (anonymous reads
 * count too), and a failure never affects reading. `seriesId` is the reader's series
 * id (`w_` work or numeric); the server normalises it. No-op offline/mock or when the
 * backend doesn't track views.
 */
export async function recordView(seriesId: string | undefined): Promise<void> {
	if (!LIVE || !seriesId || !backend.recordView) return;
	try {
		await backend.recordView(seriesId);
	} catch (err) {
		console.warn('[komika] recordView failed:', err);
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
	/** Available translators (sources) for the work; empty for single-source series. */
	translators: TranslatorOption[];
	selectedTranslatorKey: string | null;
	/** Canonical `w_` workId to switch translators against; null for single-source. */
	workId: string | null;
	/** The Suwayomi manga id of the source these chapters are read from (S2), so
	 *  prev/next stay on the same source; null = the MangaDex mirror / default. */
	readingSrc: string | null;
}

/** Honest empty reader view — no chapter/pages available for this series. */
function emptyReader(seriesId: string, tmeta: TranslatorMeta = NO_TRANSLATORS): ReaderView {
	return {
		seriesId,
		seriesTitle: '',
		chapterId: undefined,
		chNum: 0,
		chTitle: '',
		pages: [],
		chapters: [],
		prevChapterId: undefined,
		nextChapterId: undefined,
		translators: tmeta.translators,
		selectedTranslatorKey: tmeta.selectedTranslatorKey,
		workId: tmeta.workId,
		readingSrc: null,
	};
}

/** Assemble a ReaderView from a resolved chapter list + target chapter's pages. */
function buildReaderView(
	seriesId: string,
	chs: Chapter[],
	preserveOrder: boolean,
	target: Chapter,
	urls: string[],
	seriesTitle: string,
	tmeta: TranslatorMeta,
	readingSrc: string | null,
): ReaderView {
	const asc = preserveOrder ? chs : [...chs].sort((a, b) => a.number - b.number);
	const idx = asc.findIndex((c) => c.id === target.id);
	return {
		seriesId,
		seriesTitle: seriesTitle || 'Reader',
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
		translators: tmeta.translators,
		selectedTranslatorKey: tmeta.selectedTranslatorKey,
		workId: tmeta.workId,
		readingSrc,
	};
}

export function getReaderChapter(
	seriesId: string,
	chParam?: string | null,
	srcParam?: string | null,
): Promise<ReaderView> {
	return live(async () => {
		// Canonical (`w_`) works read through the selected translator (persisted
		// preference or the same default the series page picked — shared resolver).
		if (isCanonicalId(seriesId)) {
			const preferred = getPreferredTranslator(seriesId);
			const resolved = await resolveWork(seriesId, preferred);
			if (!resolved) return emptyReader(seriesId);
			// S2: a chapter may be provided by a source OTHER than the preferred
			// translator (per-chapter fallback from the aggregated list). When `src`
			// names such a source, read from it (and highlight it in the switcher);
			// otherwise use the preferred translator's already-fetched chapters.
			const selectedSuw = resolved.selected.suwayomiMangaId ?? null;
			const readFromSrc = !!srcParam && srcParam !== selectedSuw;
			const srcTranslator = readFromSrc
				? resolved.translators.find((t) => t.suwayomiMangaId === srcParam)
				: undefined;
			const tmeta: TranslatorMeta = {
				translators: resolved.translators,
				selectedTranslatorKey: srcTranslator?.key ?? resolved.selected.key,
				workId: seriesId,
			};
			const spine = readFromSrc ? false : resolved.selected.suwayomiMangaId === null;
			const chs = readFromSrc
				? await backend.chapters(srcParam as string).catch(() => [] as Chapter[])
				: resolved.chapters;
			if (!chs.length) {
				const empty = emptyReader(seriesId, tmeta);
				return { ...empty, seriesTitle: resolved.meta?.title ?? '' };
			}
			const preserveOrder = readFromSrc ? false : resolved.preserveOrder;
			const asc = preserveOrder ? chs : [...chs].sort((a, b) => a.number - b.number);
			let target = chParam ? chs.find((c) => c.id === chParam) : undefined;
			// A requested chapter not carried by this source degrades honestly to the
			// no-pages state (don't silently open a different chapter).
			if (chParam && !target) {
				const empty = emptyReader(seriesId, tmeta);
				return { ...empty, seriesTitle: resolved.meta?.title ?? '' };
			}
			if (!target) target = asc.find((c) => !c.read) ?? asc[0];
			const domainPages =
				spine && backend.canonicalPages
					? await backend.canonicalPages(target.id)
					: await backend.pages(target.id);
			const urls = await Promise.all(domainPages.map((p) => images.resolvePage(p)));
			const readingSrc = readFromSrc
				? (srcParam as string)
				: (resolved.selected.suwayomiMangaId ?? null);
			return buildReaderView(
				seriesId,
				chs,
				preserveOrder,
				target,
				urls,
				resolved.meta?.title ?? 'Reader',
				tmeta,
				readingSrc,
			);
		}
		// Numeric Suwayomi series — single source, no translator picker.
		const chs = await backend.chapters(seriesId);
		if (!chs.length) return emptyReader(seriesId);
		const asc = [...chs].sort((a, b) => a.number - b.number);
		let target = chParam ? chs.find((c) => c.id === chParam) : undefined;
		if (!target) target = asc.find((c) => !c.read) ?? asc[0];
		const domainPages = await backend.pages(target.id);
		const urls = await Promise.all(domainPages.map((p) => images.resolvePage(p)));
		const series = await backend.series(seriesId).catch(() => null);
		return buildReaderView(
			seriesId,
			chs,
			false,
			target,
			urls,
			series?.title ?? 'Reader',
			NO_TRANSLATORS,
			null,
		);
	}, emptyReader(seriesId));
}

export interface ProfileView {
	/** The signed-in user's id (empty for the signed-out sample profile). */
	id: string;
	name: string;
	handle: string;
	since: string;
	bio: string;
	badge: string;
	/** Stored avatar path/URL, or null → render an initial. */
	avatarUrl: string | null;
	stats: { value: string; label: string }[];
	reading: { id?: string; title: string; genre: string; ch: number; total: number }[];
	favGenres: { name: string; pct: number }[];
	activity: { id: string; icon: string; iconBg: string; text: string; time: string }[];
	shelves: {
		id?: string;
		title: string;
		genre: string;
		rating: string;
		shelf: string;
		favorite: boolean;
		ch: number;
		total: number;
	}[];
}

/**
 * The signed-in user's profile, built from their session identity + real reading
 * state. Resolves to `null` when signed out (or backend off/error) — the profile
 * screen renders its sign-in state.
 *
 * NOTE: reading/library reflects the shared Suwayomi state this MVP federates,
 * and "activity" is derived from that progress — there is no per-user timestamped
 * event log yet, so a true activity stream would need a new server capability.
 */
export function getProfile(): Promise<ProfileView | null> {
	return live<ProfileView | null>(async () => {
		const session = await backend.session();
		if (!session) return null;
		const user = session.user;
		const lib = await backend.library().catch(() => [] as Series[]);
		// Read progress via one batched query (not a chapters() fetch per series —
		// the old N+1 that stalled this page for a signed-in user with a large
		// library, leaving the profile stuck behind the sign-in state).
		const progress = backend.libraryProgress
			? await backend.libraryProgress().catch(() => [] as SeriesProgress[])
			: [];
		const byId = new Map(progress.map((p) => [p.id, p]));
		const rows = lib.map((s) => {
			const p = byId.get(s.id);
			const total = p?.total || s.chapterCount;
			const read = p?.read ?? 0;
			return { s, total, read };
		});
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
			// Explicit shelf wins; else derive (completed when fully read, else reading).
			shelf: (s.libraryStatus as string | null) ?? (total > 0 && read >= total ? 'completed' : 'reading'),
			favorite: s.isFavorite ?? false,
			ch: read,
			total,
		}));

		// Real, timestamped activity from the server's per-user event log, mapped to
		// display text via the library title index (falls back to a generic label
		// when the target isn't in the library, e.g. a chapter comment).
		const titleById = new Map(lib.map((s) => [s.id, s.title]));
		const rawActivity = backend.myActivity ? await backend.myActivity(12).catch(() => []) : [];
		const activity = rawActivity.map((a) => {
			const title = a.targetId ? titleById.get(a.targetId) : undefined;
			const on = title ?? (a.targetType === 'chapter' ? 'a chapter' : 'a series');
			const view =
				a.kind === 'review'
					? { icon: '★', iconBg: 'rgba(246,183,60,0.16)', text: `Reviewed ${on}` }
					: a.kind === 'library_add'
						? { icon: '＋', iconBg: 'rgba(95,191,126,0.16)', text: `Added ${on} to library` }
						: { icon: '💬', iconBg: 'rgba(255,255,255,0.07)', text: `Commented on ${on}` };
			return { id: a.id, ...view, time: relTimeAgo(a.createdAt) };
		});

		return {
			id: user.id,
			name: user.displayName?.trim() || user.username,
			handle: `@${user.username}`,
			since: `Joined ${monthYear(user.joinedAt)}`,
			bio:
				user.bio?.trim() ||
				(lib.length
					? `${chaptersRead} chapters read across ${lib.length} series in your library.`
					: 'Your library is empty — add a series to start tracking your reading.'),
			badge: user.isAdmin ? 'ADMIN' : 'READER',
			avatarUrl: user.avatarUrl,
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
	}, null);
}

/**
 * Update the signed-in user's editable profile (display name + bio). Returns the
 * refreshed session user, or `null` when no live backend supports it (mock mode).
 */
export async function updateProfile(input: {
	displayName?: string | null;
	bio?: string | null;
}): Promise<import('@komika/api').Session['user'] | null> {
	if (!LIVE || !backend.updateProfile) return null;
	return backend.updateProfile(input);
}

/**
 * Upload a new avatar image. The server squares/downscales it and re-encodes it
 * as budgeted lossless WebP on the VM data volume. Returns the new avatar URL.
 */
export async function uploadAvatar(file: Blob): Promise<string> {
	if (!LIVE || !backend.uploadAvatar) {
		throw new Error('Avatar upload requires the Komiq backend.');
	}
	return backend.uploadAvatar(file);
}

export function getSupport() {
	return Promise.resolve({ supportCategories: content.supportCategories, faqs: content.faqs });
}
