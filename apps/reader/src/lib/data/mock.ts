/**
 * Mock catalog + per-screen sample data, ported verbatim from the YOMU design
 * comps. Screens render against this until `@komika/api`'s GraphQL queries are
 * wired to the real backend — at which point these exports are swapped for
 * `backend.*` calls with the same shapes.
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

export const catalog: CatalogEntry[] = [
	{
		title: 'Vermilion Hours',
		author: 'J. Ishikawa',
		genre: 'Supernatural',
		ch: 143,
		rating: 9.3,
		status: 'ongoing',
		added: 40,
		type: 'Manga',
	},
	{
		title: 'Iron Lantern',
		author: 'Mona Vex',
		genre: 'Action',
		ch: 205,
		rating: 9.6,
		status: 'ongoing',
		added: 300,
		type: 'Manhwa',
	},
	{
		title: 'Ashfall Doctrine',
		author: 'H. Nakamura',
		genre: 'Action',
		ch: 134,
		rating: 9.4,
		status: 'ongoing',
		added: 120,
		type: 'Manga',
	},
	{
		title: 'Cinder & Sworn',
		author: 'A. Belmonte',
		genre: 'Fantasy',
		ch: 96,
		rating: 9.1,
		status: 'ongoing',
		added: 90,
		type: 'Manhua',
	},
	{
		title: 'Gravewind',
		author: 'D. Cruz',
		genre: 'Horror',
		ch: 73,
		rating: 8.9,
		status: 'ongoing',
		added: 60,
		type: 'Manhua',
	},
	{
		title: 'Hollow Bloom',
		author: 'Rei Sugawara',
		genre: 'Fantasy',
		ch: 89,
		rating: 8.8,
		status: 'ongoing',
		added: 70,
		type: 'Manhwa',
	},
	{
		title: 'Saltwater Saints',
		author: 'Yuu Harada',
		genre: 'Drama',
		ch: 34,
		rating: 8.7,
		status: 'completed',
		added: 20,
		type: 'Manga',
	},
	{
		title: 'Neon Requiem',
		author: 'K. Adachi',
		genre: 'Sci-Fi',
		ch: 62,
		rating: 8.5,
		status: 'ongoing',
		added: 30,
		type: 'Manga',
	},
	{
		title: 'The Ninth Divergence',
		author: 'L. Okonkwo',
		genre: 'Sci-Fi',
		ch: 48,
		rating: 8.4,
		status: 'completed',
		added: 25,
		type: 'Manhua',
	},
	{
		title: 'Moonlit Syntax',
		author: 'Rina Toda',
		genre: 'Romance',
		ch: 59,
		rating: 8.0,
		status: 'ongoing',
		added: 35,
		type: 'Manga',
	},
	{
		title: 'Paper Tigers',
		author: 'Sena Mori',
		genre: 'Slice of Life',
		ch: 121,
		rating: 8.2,
		status: 'ongoing',
		added: 100,
		type: 'Manga',
	},
	{
		title: 'Quiet Machines',
		author: 'Elle Park',
		genre: 'Sci-Fi',
		ch: 41,
		rating: 7.9,
		status: 'hiatus',
		added: 22,
		type: 'Manhwa',
	},
	{
		title: 'Static Bloom',
		author: 'N. Farah',
		genre: 'Romance',
		ch: 1,
		rating: 8.1,
		status: 'ongoing',
		added: 1,
		type: 'Manhwa',
	},
	{
		title: "Winter's Ledger",
		author: 'T. Vasquez',
		genre: 'Mystery',
		ch: 3,
		rating: 7.8,
		status: 'ongoing',
		added: 2,
		type: 'Manhua',
	},
	{
		title: 'The Hollow Choir',
		author: 'M. Sato',
		genre: 'Horror',
		ch: 2,
		rating: 8.3,
		status: 'ongoing',
		added: 3,
		type: 'Manga',
	},
	{
		title: 'Glass Orbit',
		author: 'Ivo Renn',
		genre: 'Sci-Fi',
		ch: 5,
		rating: 8.0,
		status: 'ongoing',
		added: 4,
		type: 'Manhwa',
	},
	{
		title: 'Rustwater',
		author: 'P. Delgado',
		genre: 'Drama',
		ch: 4,
		rating: 7.6,
		status: 'hiatus',
		added: 5,
		type: 'Manhua',
	},
	{
		title: 'Faultline',
		author: 'Kaede Ono',
		genre: 'Action',
		ch: 2,
		rating: 8.5,
		status: 'ongoing',
		added: 6,
		type: 'Manga',
	},
	{
		title: 'Emberlark',
		author: 'S. Whitfield',
		genre: 'Fantasy',
		ch: 78,
		rating: 8.6,
		status: 'completed',
		added: 88,
		type: 'Manhwa',
	},
	{
		title: 'The Long Quiet',
		author: 'H. Mercer',
		genre: 'Slice of Life',
		ch: 44,
		rating: 8.1,
		status: 'ongoing',
		added: 50,
		type: 'Manhua',
	},
	{
		title: 'Salt & Static',
		author: 'Dae Kim',
		genre: 'Romance',
		ch: 66,
		rating: 8.4,
		status: 'ongoing',
		added: 55,
		type: 'Manhwa',
	},
	{
		title: 'Nightingale Protocol',
		author: 'R. Osei',
		genre: 'Mystery',
		ch: 52,
		rating: 8.8,
		status: 'ongoing',
		added: 48,
		type: 'Manhwa',
	},
	{
		title: 'Blackthread',
		author: 'Y. Fujin',
		genre: 'Supernatural',
		ch: 91,
		rating: 9.0,
		status: 'ongoing',
		added: 80,
		type: 'Manga',
	},
	{
		title: 'The Sunless March',
		author: 'C. Aalto',
		genre: 'Action',
		ch: 118,
		rating: 9.2,
		status: 'completed',
		added: 110,
		type: 'Manhua',
	},
];

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

export function slug(title: string): string {
	return title
		.toLowerCase()
		.replace(/&/g, 'and')
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '');
}

export function typeOf(title: string): ComicType {
	return catalog.find((c) => c.title === title)?.type ?? 'Manga';
}

export function ratingOf(title: string): string {
	return (catalog.find((c) => c.title === title)?.rating ?? 8).toFixed(1);
}

// ---- Home ------------------------------------------------------------------

export const featured = [
	{ title: 'Vermilion Hours', genre: 'Supernatural', ch: 142 },
	{ title: 'Ashfall Doctrine', genre: 'Action', ch: 133 },
	{ title: 'Iron Lantern', genre: 'Action', ch: 204 },
	{ title: 'Gravewind', genre: 'Horror', ch: 72 },
	{ title: 'Cinder & Sworn', genre: 'Fantasy', ch: 95 },
];

export interface Card {
	title: string;
	ch: string;
	time: string;
	rating: string;
	cover?: string;
	id?: string;
}

export const latestUpdates: Card[] = [
	{ title: 'Hollow Bloom', ch: 'Ch. 89', time: '2h', rating: '8.8' },
	{ title: 'Neon Requiem', ch: 'Ch. 62', time: '3h', rating: '8.5' },
	{ title: 'Paper Tigers', ch: 'Ch. 121', time: '5h', rating: '8.2' },
	{ title: 'Moonlit Syntax', ch: 'Ch. 59', time: '6h', rating: '8.0' },
	{ title: 'Saltwater Saints', ch: 'Ch. 34', time: '9h', rating: '8.7' },
	{ title: 'Quiet Machines', ch: 'Ch. 41', time: '12h', rating: '7.9' },
	{ title: 'The Ninth Divergence', ch: 'Ch. 48', time: '1d', rating: '8.4' },
	{ title: 'Vermilion Hours', ch: 'Ch. 143', time: '1d', rating: '9.3' },
];

export const trending: Card[] = [
	{ title: 'Iron Lantern', ch: 'Ch. 205', time: '1d', rating: '9.6' },
	{ title: 'Ashfall Doctrine', ch: 'Ch. 134', time: '1d', rating: '9.4' },
	{ title: 'Cinder & Sworn', ch: 'Ch. 96', time: '1d', rating: '9.1' },
	{ title: 'Gravewind', ch: 'Ch. 73', time: '1d', rating: '8.9' },
	{ title: 'Hollow Bloom', ch: 'Ch. 89', time: '2h', rating: '8.8' },
	{ title: 'Vermilion Hours', ch: 'Ch. 143', time: '1d', rating: '9.3' },
];

export const latestAdded: Card[] = [
	{ title: 'Static Bloom', ch: 'Ch. 1', time: '1d', rating: '8.1' },
	{ title: "Winter's Ledger", ch: 'Ch. 3', time: '2d', rating: '7.8' },
	{ title: 'The Hollow Choir', ch: 'Ch. 2', time: '3d', rating: '8.3' },
	{ title: 'Glass Orbit', ch: 'Ch. 5', time: '4d', rating: '8.0' },
	{ title: 'Rustwater', ch: 'Ch. 4', time: '5d', rating: '7.6' },
	{ title: 'Faultline', ch: 'Ch. 2', time: '6d', rating: '8.5' },
];

export const homeGenres = [
	'Action',
	'Fantasy',
	'Sci-Fi',
	'Romance',
	'Horror',
	'Slice of Life',
	'Mystery',
	'Drama',
	'Sports',
];

export const formatCards = [
	{
		type: 'Manga' as ComicType,
		flag: '🇯🇵',
		name: 'Manga',
		desc: 'Japanese comics · read right-to-left',
		count: '1,240 titles',
		glow: 'rgba(224,131,105,0.16)',
		hover: 'rgba(224,131,105,0.4)',
	},
	{
		type: 'Manhwa' as ComicType,
		flag: '🇰🇷',
		name: 'Manhwa',
		desc: 'Korean webtoons · full-colour vertical scroll',
		count: '860 titles',
		glow: 'rgba(198,156,240,0.16)',
		hover: 'rgba(198,156,240,0.4)',
	},
	{
		type: 'Manhua' as ComicType,
		flag: '🇨🇳',
		name: 'Manhua',
		desc: 'Chinese comics · sweeping colour art',
		count: '540 titles',
		glow: 'rgba(95,200,207,0.16)',
		hover: 'rgba(95,200,207,0.4)',
	},
];

export const searchFilterSections = [
	{ label: 'Sort by', options: ['Trending', 'Newest', 'Top Rated', 'Most Chapters', 'A–Z'] },
	{ label: 'Genre', options: ALL_GENRES },
	{ label: 'Status', options: ['Ongoing', 'Completed', 'Hiatus'] },
	{ label: 'Minimum rating', options: ['7.0+', '8.0+', '9.0+'] },
];

// ---- Series detail (Vermilion Hours) ---------------------------------------

export const seriesDetail = {
	title: 'Vermilion Hours',
	author: 'Story by J. Ishikawa',
	artist: 'Art by M. Sato',
	status: 'ONGOING · UPDATES WEEKLY',
	type: 'Manga',
	flag: '🇯🇵',
	rating: '9.3',
	votes: '12.4K',
	chapters: 143,
	followers: '128K',
	updated: '1d ago',
	cta: 'Continue Ch. 62',
	continueCh: 62,
	genres: ['Supernatural', 'Action', 'Seinen', 'Dark Fantasy', 'Psychological'],
	synopsis:
		"Ren Asakura can borrow the forgotten powers of the dead — for a price paid in hours of his own life. Each bargain buys him a little more strength and costs him a little more of the future he was supposed to have. As the debts stack and the collectors grow impatient, Ren must decide how much of himself he's willing to spend before there's nothing left to reclaim. A slow-burning supernatural thriller about memory, sacrifice, and the true cost of borrowed time.",
};

const CHAPTER_TITLES = [
	'The Debt Comes Due',
	'Borrowed Light',
	'A Name in Ash',
	'The Ninth Hour',
	'Where the Dead Keep Time',
	'Vermilion',
	'Small Mercies',
	'The Ledger',
	'Half a Life',
	'What Ren Owes',
	"The Collector's Terms",
	'Under the Red Lamp',
	'Interest Accrued',
	'The Long Repayment',
	'Nightfall Accounts',
	'Two Debts',
	'The Hour That Remains',
	'Threadbare',
	'A Quiet Foreclosure',
	'The Last Loan',
];

export interface Chapter {
	n: number;
	title: string;
	date: string;
	isNew: boolean;
	read: boolean;
}

export function buildChapters(total = seriesDetail.chapters, readUpTo = 61): Chapter[] {
	const N = total;
	const arr: Chapter[] = [];
	for (let i = N; i >= 1; i--) {
		const idxFromTop = N - i;
		const title = CHAPTER_TITLES[(i * 7) % CHAPTER_TITLES.length];
		const days = idxFromTop === 0 ? 0 : idxFromTop <= 2 ? idxFromTop : idxFromTop * 3;
		let date: string;
		if (days === 0) date = 'Today';
		else if (days === 1) date = 'Yesterday';
		else if (days < 30) date = days + ' days ago';
		else {
			const mo = Math.round(days / 30);
			date = mo + (mo === 1 ? ' month ago' : ' months ago');
		}
		arr.push({
			n: i,
			title: `Ch. ${i} · ${title}`,
			date,
			isNew: idxFromTop < 3,
			read: i <= readUpTo,
		});
	}
	return arr;
}

export function chapterTitle(n: number): string {
	return CHAPTER_TITLES[(n * 7) % CHAPTER_TITLES.length];
}

// Shape of a locally-persisted series review in the offline/backend-off fallback.
// No seeded data — the fallback starts empty (see CATALOGUE.md §7).
export interface SeriesComment {
	id: number;
	name: string;
	initial: string;
	chapter: string;
	time: string;
	body: string;
	likes: number;
	liked: boolean;
}

export const relatedSeries = [
	{ title: 'Ashfall Doctrine', genre: 'Action', ch: 133, rating: '9.4' },
	{ title: 'Gravewind', genre: 'Horror', ch: 72, rating: '8.9' },
	{ title: 'Hollow Bloom', genre: 'Fantasy', ch: 89, rating: '8.8' },
	{ title: 'Neon Requiem', genre: 'Sci-Fi', ch: 62, rating: '8.5' },
	{ title: 'Cinder & Sworn', genre: 'Fantasy', ch: 96, rating: '9.1' },
	{ title: 'Iron Lantern', genre: 'Action', ch: 205, rating: '9.6' },
	{ title: 'Saltwater Saints', genre: 'Drama', ch: 34, rating: '8.7' },
];

// ---- Reader ----------------------------------------------------------------

export interface ReaderPage {
	n: number;
	label: string;
	ratio: string;
	dim: string;
}

export function readerPages(): ReaderPage[] {
	const arr: ReaderPage[] = [];
	for (let i = 1; i <= 11; i++) {
		const spread = i === 6;
		arr.push({
			n: i,
			label: String(i).padStart(2, '0'),
			ratio: spread ? '1600 / 1150' : '800 / 1200',
			dim: spread ? '1600×1150' : '800×1200',
		});
	}
	return arr;
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
	likes: number;
	liked: boolean;
	replies: number;
	body: string;
}

// ---- Library ---------------------------------------------------------------

export type Shelf = 'reading' | 'completed' | 'onhold' | 'plan';

export interface LibraryEntry {
	title: string;
	genre: string;
	rating: string;
	shelf: Shelf;
	read: number;
	total: number;
}

export const libraryCatalog: LibraryEntry[] = [
	{
		title: 'Vermilion Hours',
		genre: 'Supernatural',
		rating: '9.3',
		shelf: 'reading',
		read: 62,
		total: 143,
	},
	{
		title: 'Iron Lantern',
		genre: 'Action',
		rating: '9.6',
		shelf: 'reading',
		read: 180,
		total: 205,
	},
	{ title: 'Hollow Bloom', genre: 'Fantasy', rating: '8.8', shelf: 'reading', read: 61, total: 89 },
	{ title: 'Neon Requiem', genre: 'Sci-Fi', rating: '8.5', shelf: 'reading', read: 12, total: 62 },
	{ title: 'Gravewind', genre: 'Horror', rating: '8.9', shelf: 'reading', read: 70, total: 73 },
	{
		title: 'Ashfall Doctrine',
		genre: 'Action',
		rating: '9.4',
		shelf: 'completed',
		read: 133,
		total: 133,
	},
	{
		title: 'Saltwater Saints',
		genre: 'Drama',
		rating: '8.7',
		shelf: 'completed',
		read: 34,
		total: 34,
	},
	{
		title: 'The Ninth Divergence',
		genre: 'Sci-Fi',
		rating: '8.4',
		shelf: 'completed',
		read: 48,
		total: 48,
	},
	{
		title: 'Cinder & Sworn',
		genre: 'Fantasy',
		rating: '9.1',
		shelf: 'onhold',
		read: 40,
		total: 96,
	},
	{
		title: 'Paper Tigers',
		genre: 'Slice of Life',
		rating: '8.2',
		shelf: 'onhold',
		read: 9,
		total: 121,
	},
	{ title: 'Moonlit Syntax', genre: 'Romance', rating: '8.0', shelf: 'plan', read: 0, total: 59 },
	{ title: 'Quiet Machines', genre: 'Sci-Fi', rating: '7.9', shelf: 'plan', read: 0, total: 41 },
	{ title: 'Static Bloom', genre: 'Mystery', rating: '8.1', shelf: 'plan', read: 0, total: 24 },
	{ title: 'Glass Orbit', genre: 'Sci-Fi', rating: '8.0', shelf: 'plan', read: 0, total: 30 },
];

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

export const continueRow = [
	{ title: 'Vermilion Hours', ch: 'Ch. 62', progress: 43, genre: 'Supernatural' },
	{ title: 'Iron Lantern', ch: 'Ch. 180', progress: 88, genre: 'Action' },
	{ title: 'Gravewind', ch: 'Ch. 70', progress: 96, genre: 'Horror' },
	{ title: 'Hollow Bloom', ch: 'Ch. 61', progress: 69, genre: 'Fantasy' },
	{ title: 'Neon Requiem', ch: 'Ch. 12', progress: 19, genre: 'Sci-Fi' },
];

// ---- Updates ---------------------------------------------------------------

export const trendingGroups: { label: string; items: Card[] }[] = [
	{
		label: 'Trending Today',
		items: [
			{ title: 'Iron Lantern', ch: 'Ch. 205', time: '1d', rating: '9.6' },
			{ title: 'Ashfall Doctrine', ch: 'Ch. 134', time: '1d', rating: '9.4' },
			{ title: 'Vermilion Hours', ch: 'Ch. 143', time: '1d', rating: '9.3' },
			{ title: 'Cinder & Sworn', ch: 'Ch. 96', time: '1d', rating: '9.1' },
			{ title: 'Gravewind', ch: 'Ch. 73', time: '1d', rating: '8.9' },
			{ title: 'Hollow Bloom', ch: 'Ch. 89', time: '2h', rating: '8.8' },
		],
	},
	{
		label: 'Trending This Week',
		items: [
			{ title: 'Cinder & Sworn', ch: 'Ch. 96', time: '1d', rating: '9.5' },
			{ title: 'Vermilion Hours', ch: 'Ch. 143', time: '1d', rating: '9.4' },
			{ title: 'Hollow Bloom', ch: 'Ch. 89', time: '2h', rating: '9.2' },
			{ title: 'Iron Lantern', ch: 'Ch. 205', time: '1d', rating: '9.0' },
			{ title: 'Saltwater Saints', ch: 'Ch. 34', time: '9h', rating: '8.7' },
			{ title: 'Neon Requiem', ch: 'Ch. 62', time: '3h', rating: '8.5' },
		],
	},
];

export const newUpdates: Card[] = [
	{ title: 'Hollow Bloom', ch: 'Ch. 89', time: '2h', rating: '8.8' },
	{ title: 'Neon Requiem', ch: 'Ch. 62', time: '3h', rating: '8.5' },
	{ title: 'Paper Tigers', ch: 'Ch. 121', time: '5h', rating: '8.2' },
	{ title: 'Moonlit Syntax', ch: 'Ch. 59', time: '6h', rating: '8.0' },
	{ title: 'Saltwater Saints', ch: 'Ch. 34', time: '9h', rating: '8.7' },
	{ title: 'Quiet Machines', ch: 'Ch. 41', time: '12h', rating: '7.9' },
	{ title: 'The Ninth Divergence', ch: 'Ch. 48', time: '1d', rating: '8.4' },
	{ title: 'Vermilion Hours', ch: 'Ch. 143', time: '1d', rating: '9.3' },
	{ title: 'Iron Lantern', ch: 'Ch. 205', time: '1d', rating: '9.6' },
	{ title: 'Gravewind', ch: 'Ch. 73', time: '1d', rating: '8.9' },
	{ title: 'Cinder & Sworn', ch: 'Ch. 96', time: '1d', rating: '9.1' },
	{ title: 'Ashfall Doctrine', ch: 'Ch. 134', time: '1d', rating: '9.4' },
];

export const hotUpdates: Card[] = [
	{ title: 'Iron Lantern', ch: 'Ch. 205', time: '1d', rating: '9.6' },
	{ title: 'Cinder & Sworn', ch: 'Ch. 96', time: '1d', rating: '9.1' },
	{ title: 'Vermilion Hours', ch: 'Ch. 143', time: '1d', rating: '9.3' },
	{ title: 'Ashfall Doctrine', ch: 'Ch. 134', time: '1d', rating: '9.4' },
	{ title: 'Hollow Bloom', ch: 'Ch. 89', time: '2h', rating: '8.8' },
	{ title: 'Gravewind', ch: 'Ch. 73', time: '1d', rating: '8.9' },
	{ title: 'Neon Requiem', ch: 'Ch. 62', time: '3h', rating: '8.5' },
	{ title: 'Paper Tigers', ch: 'Ch. 121', time: '5h', rating: '8.2' },
	{ title: 'Saltwater Saints', ch: 'Ch. 34', time: '9h', rating: '8.7' },
	{ title: 'Moonlit Syntax', ch: 'Ch. 59', time: '6h', rating: '8.0' },
	{ title: 'Quiet Machines', ch: 'Ch. 41', time: '12h', rating: '7.9' },
	{ title: 'The Ninth Divergence', ch: 'Ch. 48', time: '1d', rating: '8.4' },
];

// ---- Profile ---------------------------------------------------------------

export const profile = {
	name: 'Kai Reyes',
	handle: '@kaireads',
	since: 'Member since March 2023',
	bio: 'Seinen and dark-fantasy devotee. Always three chapters behind and perfectly fine with it. Currently obsessed with slow-burn supernatural thrillers.',
	badge: 'PRO READER',
	stats: [
		{ value: '128', label: 'Series read' },
		{ value: '3,412', label: 'Chapters read' },
		{ value: '5', label: 'Reading now' },
		{ value: '8.7', label: 'Avg rating' },
		{ value: '47', label: 'Day streak' },
	],
	reading: [
		{ title: 'Vermilion Hours', genre: 'Supernatural', ch: 62, total: 143 },
		{ title: 'Iron Lantern', genre: 'Action', ch: 180, total: 205 },
		{ title: 'Gravewind', genre: 'Horror', ch: 70, total: 73 },
		{ title: 'Neon Requiem', genre: 'Sci-Fi', ch: 12, total: 62 },
	],
	favGenres: [
		{ name: 'Action', pct: 32 },
		{ name: 'Fantasy', pct: 24 },
		{ name: 'Sci-Fi', pct: 18 },
		{ name: 'Horror', pct: 14 },
		{ name: 'Drama', pct: 12 },
	],
	activity: [
		{
			icon: '📖',
			iconBg: 'rgba(255,255,255,0.07)',
			text: 'Read Ch. 62 of Vermilion Hours',
			time: '2 hours ago',
		},
		{
			icon: '★',
			iconBg: 'rgba(246,183,60,0.16)',
			text: 'Rated Ashfall Doctrine 9 / 10',
			time: 'Yesterday',
		},
		{
			icon: '＋',
			iconBg: 'rgba(95,191,126,0.16)',
			text: 'Added Saltwater Saints to Library',
			time: '2 days ago',
		},
		{
			icon: '✓',
			iconBg: 'rgba(95,191,126,0.16)',
			text: 'Finished The Ninth Divergence',
			time: '4 days ago',
		},
		{
			icon: '📖',
			iconBg: 'rgba(255,255,255,0.07)',
			text: 'Read Ch. 180 of Iron Lantern',
			time: '5 days ago',
		},
	],
	shelves: [
		{
			title: 'Vermilion Hours',
			genre: 'Supernatural',
			rating: '9.3',
			shelf: 'reading',
			ch: 62,
			total: 143,
		},
		{
			title: 'Iron Lantern',
			genre: 'Action',
			rating: '9.6',
			shelf: 'reading',
			ch: 180,
			total: 205,
		},
		{ title: 'Hollow Bloom', genre: 'Fantasy', rating: '8.8', shelf: 'reading', ch: 61, total: 89 },
		{ title: 'Neon Requiem', genre: 'Sci-Fi', rating: '8.5', shelf: 'reading', ch: 12, total: 62 },
		{ title: 'Gravewind', genre: 'Horror', rating: '8.9', shelf: 'reading', ch: 70, total: 73 },
		{
			title: 'Ashfall Doctrine',
			genre: 'Action',
			rating: '9.4',
			shelf: 'completed',
			ch: 133,
			total: 133,
		},
		{
			title: 'Saltwater Saints',
			genre: 'Drama',
			rating: '8.7',
			shelf: 'completed',
			ch: 34,
			total: 34,
		},
		{
			title: 'The Ninth Divergence',
			genre: 'Sci-Fi',
			rating: '8.4',
			shelf: 'completed',
			ch: 48,
			total: 48,
		},
		{
			title: 'Cinder & Sworn',
			genre: 'Fantasy',
			rating: '9.1',
			shelf: 'completed',
			ch: 96,
			total: 96,
		},
		{
			title: 'Iron Lantern',
			genre: 'Action',
			rating: '9.6',
			shelf: 'favorites',
			ch: 205,
			total: 205,
		},
		{
			title: 'Vermilion Hours',
			genre: 'Supernatural',
			rating: '9.3',
			shelf: 'favorites',
			ch: 143,
			total: 143,
		},
		{
			title: 'Ashfall Doctrine',
			genre: 'Action',
			rating: '9.4',
			shelf: 'favorites',
			ch: 133,
			total: 133,
		},
	],
};

// ---- Donate ----------------------------------------------------------------

export const donateTiers = [
	{
		key: 'supporter',
		name: 'Supporter',
		price: '$3',
		accent: 'var(--k-ongoing)',
		popular: false,
		perks: [
			'Ad-free reading, forever',
			'Supporter badge on your profile',
			'Early access to new chapters',
		],
	},
	{
		key: 'patron',
		name: 'Patron',
		price: '$8',
		accent: 'var(--k-hiatus)',
		popular: true,
		perks: [
			'Everything in Supporter',
			'Unlimited offline downloads',
			'Vote on which series get licensed next',
			'Members-only comment lounge',
		],
	},
	{
		key: 'founder',
		name: 'Founder',
		price: '$20',
		accent: 'var(--k-accent-purple)',
		popular: false,
		perks: [
			'Everything in Patron',
			'Name in the credits page',
			'Two guest passes to gift friends',
			'Direct line to the YOMU team',
		],
	},
];

export const donateAmounts = [5, 15, 30, 50, 100];

export const donateAllocation = [
	{
		pct: '62%',
		color: 'var(--k-accent)',
		label: 'Creators & translators',
		desc: 'Paid directly to the artists, writers, and localization teams behind each series.',
	},
	{
		pct: '28%',
		color: 'var(--k-hiatus)',
		label: 'Servers & delivery',
		desc: 'Fast, global image hosting so pages load instantly, anywhere.',
	},
	{
		pct: '10%',
		color: 'var(--k-ongoing)',
		label: 'Platform & tools',
		desc: 'A small team keeping YOMU running, safe, and ad-free.',
	},
];

// ---- Support ---------------------------------------------------------------

export const supportCategories = [
	{
		icon: 'user',
		title: 'Account',
		desc: 'Sign-in, profile, and privacy settings.',
		count: 14,
		iconBg: 'rgba(127,211,154,0.14)',
		iconColor: 'var(--k-ongoing)',
	},
	{
		icon: 'book',
		title: 'Reading',
		desc: 'Reader modes, downloads, and formats.',
		count: 21,
		iconBg: 'rgba(198,156,240,0.14)',
		iconColor: 'var(--k-accent-purple)',
	},
	{
		icon: 'card',
		title: 'Billing & membership',
		desc: 'Donations, tiers, and receipts.',
		count: 9,
		iconBg: 'rgba(224,179,84,0.14)',
		iconColor: 'var(--k-hiatus)',
	},
	{
		icon: 'flag',
		title: 'Report an issue',
		desc: 'Broken pages, wrong info, or abuse.',
		count: 7,
		iconBg: 'rgba(224,131,105,0.14)',
		iconColor: 'var(--k-accent)',
	},
];

export const faqs = [
	{
		q: 'Is YOMU free to use?',
		a: 'Yes. Reading is free and always will be. Members who donate get perks like ad-free browsing, offline downloads, and early chapters, but every series is readable without paying.',
	},
	{
		q: "What's the difference between manga, manhwa, and manhua?",
		a: 'They refer to comics from different regions: manga (Japan, read right-to-left), manhwa (Korea, usually full-color vertical scroll), and manhua (China). You can filter by any of these formats on the Browse page.',
	},
	{
		q: 'How do I download chapters to read offline?',
		a: "Offline downloads are a Patron-tier perk. Once you're a member, tap the download icon on any chapter or series page and it'll be available in your Library without a connection.",
	},
	{
		q: 'Can I change the reading direction and page size?',
		a: 'Absolutely. Open any chapter and tap the settings icon to switch between long-strip and single-page modes and adjust page width. Your preferences are remembered per device.',
	},
	{
		q: 'How do I cancel or change my membership?',
		a: 'Go to your Profile settings and open Membership. You can upgrade, downgrade, or cancel at any time — changes take effect at the end of your current billing cycle.',
	},
	{
		q: "A chapter is missing pages or won't load. What do I do?",
		a: 'First try refreshing. If it persists, use the Report an issue option on the chapter page so our team can re-process the file, or reach out via live chat below.',
	},
];
