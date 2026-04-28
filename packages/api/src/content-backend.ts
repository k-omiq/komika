import type { Chapter, Id, Page, Series } from '@komika/types';

/** Identifies a specific series within a specific source, resolved from `workSources`. */
export interface SourceRef {
	/** Suwayomi source id (or "mangadex"). */
	sourceId: string;
	/** Manga id/slug within that source. */
	sourceKey: string;
}

/**
 * The content-only slice of a backend: live series/chapters/pages fetched from a
 * specific source mapping. Implemented on-device by the embedded engine
 * (LocalSuwayomiBackend). Deliberately excludes auth/library/progress/social —
 * those always route to the hosted server (see the §1 routing table).
 */
export interface ContentBackend {
	/** Whether the local engine is up and can serve content right now. */
	isReady(): Promise<boolean>;
	/** Live series metadata for one source mapping. */
	series(ref: SourceRef): Promise<Series>;
	/** Live chapter list for one source mapping. */
	chapters(ref: SourceRef): Promise<Chapter[]>;
	/** Page image URLs for a chapter (chapterId is source-local). */
	pages(chapterId: Id): Promise<Page[]>;
}
