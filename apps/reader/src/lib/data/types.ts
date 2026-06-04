/**
 * Presentation types, constants, and tiny formatting utils shared by the
 * screens and components. Pure view metadata — no catalog/sample data lives
 * here; all content comes from the backend via `source.ts`.
 */

export type ComicType = 'Manga' | 'Manhwa' | 'Manhua';
export type Status = 'ongoing' | 'completed' | 'hiatus';

export const FLAG: Record<ComicType, string> = {
	Manga: '🇯🇵',
	Manhwa: '🇰🇷',
	Manhua: '🇨🇳',
};

export const TYPE_COLOR: Record<ComicType, string> = {
	Manga: 'var(--k-accent)',
	Manhwa: 'var(--k-accent-purple)',
	Manhua: 'var(--k-accent-teal)',
};

export const STATUS_META: Record<Status, { label: string; color: string }> = {
	ongoing: { label: 'Ongoing', color: 'var(--k-ongoing)' },
	completed: { label: 'Completed', color: 'var(--k-completed)' },
	hiatus: { label: 'Hiatus', color: 'var(--k-hiatus)' },
};

/** A browse-catalog row as the Browse screen renders it. */
export interface CatalogEntry {
	title: string;
	author: string;
	genre: string;
	ch: number;
	rating: number;
	status: Status;
	added: number;
	type: ComicType;
	cover?: string;
	id?: string;
}

/** A rail/grid card as the Home and Updates screens render it. */
export interface Card {
	title: string;
	ch: string;
	time: string;
	rating: string;
	cover?: string;
	id?: string;
	/** Format of the series, when the source feed knows it. */
	type?: ComicType;
}

/** Genre filter chips on the Browse screen. */
export const ALL_GENRES = [
	'Action',
	'Fantasy',
	'Sci-Fi',
	'Romance',
	'Horror',
	'Slice of Life',
	'Mystery',
	'Drama',
	'Supernatural',
];

/** URL-safe slug for title-based series links (fallback when no id is known). */
export function slug(title: string): string {
	return title
		.toLowerCase()
		.replace(/&/g, 'and')
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '');
}

/**
 * "Browse by format" card chrome (name, blurb, glow colours). Counts are
 * derived from the live catalog sample in `source.ts`; the empty default
 * renders no count line.
 */
export const FORMAT_CARDS = [
	{
		type: 'Manga' as ComicType,
		flag: '🇯🇵',
		name: 'Manga',
		desc: 'Japanese comics · read right-to-left',
		count: '',
		glow: 'rgba(224,131,105,0.16)',
		hover: 'rgba(224,131,105,0.4)',
	},
	{
		type: 'Manhwa' as ComicType,
		flag: '🇰🇷',
		name: 'Manhwa',
		desc: 'Korean webtoons · full-colour vertical scroll',
		count: '',
		glow: 'rgba(198,156,240,0.16)',
		hover: 'rgba(198,156,240,0.4)',
	},
	{
		type: 'Manhua' as ComicType,
		flag: '🇨🇳',
		name: 'Manhua',
		desc: 'Chinese comics · sweeping colour art',
		count: '',
		glow: 'rgba(95,200,207,0.16)',
		hover: 'rgba(95,200,207,0.4)',
	},
];

/** Advanced-search panel sections (SearchOverlay presentation config). */
export const searchFilterSections = [
	{ label: 'Sort by', options: ['Trending', 'Newest', 'Top Rated', 'Most Chapters', 'A–Z'] },
	{ label: 'Genre', options: ALL_GENRES },
	{ label: 'Status', options: ['Ongoing', 'Completed', 'Hiatus'] },
	{ label: 'Minimum rating', options: ['7.0+', '8.0+', '9.0+'] },
];

// ---- Library shelves ---------------------------------------------------------

export type Shelf = 'reading' | 'completed' | 'onhold' | 'plan';

export const SHELF_META: Record<
	Shelf,
	{ label: string; color: string; bg: string; border: string }
> = {
	reading: {
		label: 'READING',
		color: 'var(--k-ongoing)',
		bg: 'rgba(95,191,126,0.16)',
		border: 'rgba(95,191,126,0.4)',
	},
	completed: {
		label: 'DONE',
		color: 'var(--k-on-primary)',
		bg: 'var(--k-primary)',
		border: 'var(--k-primary)',
	},
	onhold: {
		label: 'ON HOLD',
		color: '#e6c06a',
		bg: 'rgba(246,183,60,0.14)',
		border: 'rgba(246,183,60,0.4)',
	},
	plan: {
		label: 'PLAN',
		color: 'var(--k-text-3)',
		bg: 'rgba(255,255,255,0.08)',
		border: 'rgba(255,255,255,0.18)',
	},
};

// ---- Social view shapes --------------------------------------------------------

// Shape of a locally-persisted series review in the offline/backend-off fallback.
// No seeded data — the fallback starts empty (see CATALOGUE.md §7).
export interface SeriesComment {
	id: number;
	name: string;
	initial: string;
	chapter: string;
	time: string;
	body: string;
	hasSpoiler: boolean;
	likes: number;
	liked: boolean;
}

// Shape of a locally-persisted chapter comment in the offline/backend-off fallback.
// No seeded data — the fallback starts empty (see CATALOGUE.md §7).
export interface ReaderComment {
	id: string;
	name: string;
	initial: string;
	bg: string;
	fg: string;
	ts: number;
	time: string;
	isOp: boolean;
	hasSpoiler: boolean;
	likes: number;
	liked: boolean;
	replies: number;
	body: string;
}
