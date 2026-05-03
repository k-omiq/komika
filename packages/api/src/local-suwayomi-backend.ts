import type { Chapter, Id, Page, Series, SeriesStatus, ComicType } from '@komika/types';
import type { ContentBackend, SourceRef } from './content-backend.js';
import { isTauri } from './platform.js';

/**
 * Content-only adapter to the EMBEDDED Suwayomi engine, reached over an in-process
 * IPC transport (a Tauri `suwayomi_gql` command) instead of an HTTP `fetch`.
 *
 * This is the on-device counterpart to {@link SuwayomiBackend}: it speaks the same
 * Suwayomi GraphQL schema and maps it onto `@komika/types`, but it never opens a
 * network socket — the composite backend routes live series/chapters/pages here
 * when the embedded engine is up, while auth/library/progress/social always stay
 * on the hosted server. It deliberately implements ONLY {@link ContentBackend}.
 *
 * Status: `isReady()` is LIVE — it queries the `suwayomi_status` Tauri command and
 * returns true only when the embedded engine reports `state === "ready"`. Whether
 * the composite backend ever routes content here still depends on a local backend
 * being constructed at all (gated by the `PUBLIC_KOMIKA_NATIVE_ENGINE` flag under
 * Tauri) and on the Wave C content-routing branches being wired.
 */
export class LocalSuwayomiBackend implements ContentBackend {
	/**
	 * Execute a GraphQL document against the embedded engine over IPC.
	 *
	 * NOTE: the `suwayomi_gql` Tauri command is registered in Wave C. Until then
	 * this path is UNREACHABLE because {@link isReady} returns false, so the
	 * composite backend never routes content calls here. `invoke` is imported
	 * dynamically (mirroring image-provider.ts) so this package keeps building for
	 * the web target, where `@tauri-apps/api` is absent at runtime.
	 */
	private async gql<T>(query: string, variables?: Record<string, unknown>): Promise<T> {
		const { invoke } = await import('@tauri-apps/api/core');
		const json = await invoke<{ data?: T; errors?: { message: string }[] }>('suwayomi_gql', {
			query,
			variables,
		});
		if (json.errors?.length) throw new Error(json.errors.map((e) => e.message).join('; '));
		if (json.data == null) throw new Error('Suwayomi returned no data');
		return json.data;
	}

	/**
	 * Whether the embedded engine is up and can serve content right now.
	 *
	 * Queries the `suwayomi_status` Tauri command and returns true iff the engine
	 * reports `state === "ready"`; any other state (`starting` / `degraded` /
	 * `stopped`) yields false. Never throws: on the web target `@tauri-apps/api` is
	 * absent, so we short-circuit via {@link isTauri} before importing it, and any
	 * failure of the invoke (engine not up, command missing) is swallowed as false.
	 */
	async isReady(): Promise<boolean> {
		if (!isTauri()) return false;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const status = await invoke<{ state?: string }>('suwayomi_status');
			return status?.state === 'ready';
		} catch {
			return false;
		}
	}

	async series(ref: SourceRef): Promise<Series> {
		const d = await this.gql<{ fetchSourceManga: { mangas: SuwayomiManga[] } }>(FETCH_SOURCE_MANGA, {
			source: ref.sourceId,
			key: ref.sourceKey,
		});
		const manga = d.fetchSourceManga?.mangas?.[0];
		if (!manga) throw new Error(`Suwayomi: no manga for ${ref.sourceId}/${ref.sourceKey}`);
		return mapManga(manga);
	}

	async chapters(ref: SourceRef): Promise<Chapter[]> {
		const d = await this.gql<{ fetchSourceChapters: { chapters: SuwayomiChapter[] } }>(
			FETCH_CHAPTERS,
			{ source: ref.sourceId, key: ref.sourceKey },
		);
		return (d.fetchSourceChapters?.chapters ?? []).map((c) => mapChapter(c));
	}

	async pages(chapterId: Id): Promise<Page[]> {
		const d = await this.gql<{ fetchChapterPages: { pages: string[] } }>(FETCH_PAGES, {
			id: Number(chapterId),
		});
		// No base-URL rewrite here: the engine is reached over IPC, not an HTTP base
		// URL, so page URLs are passed through as-is. The NativeImageProvider /
		// local proxy is responsible for turning them into displayable bytes.
		return (d.fetchChapterPages?.pages ?? []).map((url, index) => ({ index, sourceUrl: url }));
	}
}

// --- Suwayomi GraphQL shapes (re-declared subset, mirroring suwayomi-backend.ts) ---

const MANGA_FIELDS = /* GraphQL */ `
	fragment MangaFields on MangaType {
		id
		title
		thumbnailUrl
		author
		artist
		description
		genre
		status
		inLibrary
		inLibraryAt
		lastFetchedAt
		sourceId
		source {
			lang
		}
		chapters {
			totalCount
		}
	}
`;

const CHAPTER_FIELDS = /* GraphQL */ `
	fragment ChapterFields on ChapterType {
		id
		mangaId
		name
		chapterNumber
		scanlator
		uploadDate
		isRead
		isBookmarked
		isDownloaded
		lastPageRead
		pageCount
	}
`;

// Wave C: confirm exact Suwayomi source-fetch query shape. Fetch a SPECIFIC manga
// by (source id, source-local key) rather than auto-picking a source.
const FETCH_SOURCE_MANGA = /* GraphQL */ `
	${MANGA_FIELDS}
	mutation FetchSourceManga($source: LongString!, $key: String!) {
		fetchSourceManga(input: { source: $source, key: $key }) {
			mangas {
				...MangaFields
			}
		}
	}
`;

// Wave C: confirm exact Suwayomi source-fetch query shape.
const FETCH_CHAPTERS = /* GraphQL */ `
	${CHAPTER_FIELDS}
	mutation FetchSourceChapters($source: LongString!, $key: String!) {
		fetchSourceChapters(input: { source: $source, key: $key }) {
			chapters {
				...ChapterFields
			}
		}
	}
`;

const FETCH_PAGES = /* GraphQL */ `
	mutation FetchPages($id: Int!) {
		fetchChapterPages(input: { chapterId: $id }) {
			pages
		}
	}
`;

interface SuwayomiManga {
	id: number;
	title: string;
	thumbnailUrl: string | null;
	author: string | null;
	artist: string | null;
	description: string | null;
	genre: string[];
	status: string;
	inLibrary: boolean;
	inLibraryAt: string | null;
	lastFetchedAt: string | null;
	sourceId: string;
	source: { lang: string } | null;
	chapters: { totalCount: number };
}

interface SuwayomiChapter {
	id: number;
	mangaId: number;
	name: string;
	chapterNumber: number;
	scanlator: string | null;
	uploadDate: string | null;
	isRead: boolean;
	isBookmarked: boolean;
	isDownloaded: boolean;
	lastPageRead: number;
	pageCount: number;
}

const STATUS_MAP: Record<string, SeriesStatus> = {
	ONGOING: 'ONGOING',
	COMPLETED: 'COMPLETED',
	PUBLISHING_FINISHED: 'COMPLETED',
	LICENSED: 'COMPLETED',
	CANCELLED: 'CANCELLED',
	ON_HIATUS: 'HIATUS',
	UNKNOWN: 'UNKNOWN',
};

/** Map a source language to a Komika comic type (best-effort). */
function typeFromLang(lang: string | undefined): ComicType {
	if (!lang) return 'MANGA';
	const l = lang.toLowerCase();
	if (l.startsWith('ko')) return 'MANHWA';
	if (l.startsWith('zh')) return 'MANHUA';
	return 'MANGA';
}

/** Suwayomi timestamps are epoch (seconds or millis, as strings). Coerce to ISO. */
function toIso(v: string | null): string {
	if (!v) return '';
	const n = Number(v);
	if (!Number.isFinite(n) || n <= 0) return '';
	const ms = n > 1e12 ? n : n * 1000;
	try {
		return new Date(ms).toISOString();
	} catch {
		return '';
	}
}

function mapManga(m: SuwayomiManga): Series {
	return {
		id: String(m.id),
		title: m.title,
		altTitles: [],
		author: m.author,
		artist: m.artist,
		description: m.description,
		genres: m.genre ?? [],
		type: typeFromLang(m.source?.lang),
		status: STATUS_MAP[m.status] ?? 'UNKNOWN',
		// No base-URL rewrite: cover bytes are resolved by the image provider.
		coverUrl: m.thumbnailUrl ?? '',
		sourceId: String(m.sourceId),
		chapterCount: m.chapters?.totalCount ?? 0,
		isMarked: m.inLibrary,
		isNsfw: false, // canonical NSFW flag is a Komika service; Suwayomi has none
		rating: { average: 0, count: 0, distribution: [] },
		scan: {
			avgIntervalHours: 0,
			overrideIntervalHours: null,
			pollEveryMinutes: 30,
			pollEveryMinutesOverride: null,
			paused: m.status === 'COMPLETED' || m.status === 'ON_HIATUS',
			// The Suwayomi adapter has no Komika admin overrides.
			statusOverride: null,
			pausedOverride: null,
			lastScannedAt: toIso(m.lastFetchedAt),
			nextScanAt: null,
		},
		createdAt: toIso(m.inLibraryAt),
		updatedAt: toIso(m.lastFetchedAt),
	};
}

function mapChapter(c: SuwayomiChapter): Chapter {
	return {
		id: String(c.id),
		seriesId: String(c.mangaId),
		number: c.chapterNumber,
		title: c.name,
		pageCount: c.pageCount,
		uploadedAt: toIso(c.uploadDate),
		scanlator: c.scanlator,
		read: c.isRead,
		lastPageRead: c.lastPageRead,
		bookmarked: c.isBookmarked,
		isDownloaded: c.isDownloaded,
	};
}

export function createLocalSuwayomiBackend(): ContentBackend {
	return new LocalSuwayomiBackend();
}
