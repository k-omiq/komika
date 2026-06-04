/**
 * Social repository — the seam the Series/Reader screens use for ratings,
 * reviews, and per-chapter comments.
 *
 * When the unified Komika backend is active AND the reader is signed in, this
 * reads/writes the real multi-user social layer (`backend.reviews / comments /
 * postReview / postComment`). Otherwise it falls back to the single-device
 * localStorage store (`./social.ts`) so the app still works standalone against
 * the Suwayomi adapter or pure mock — the same "always renderable" contract the
 * data repository (`source.ts`) follows.
 *
 * Mapping notes:
 *  - The backend has per-series **reviews** (score 1–10 + body) and per-chapter
 *    **comments**. The Series page's rating widget upserts the user's review
 *    SCORE (body may be empty = a pure rating); its comment thread shows the
 *    body-bearing reviews. The reader's per-chapter 1–5 rating has no backend
 *    field, so it stays local (see the reader screen).
 *  - Likes/replies aren't modelled server-side; in live mode they're ephemeral
 *    client-only affordances (reset on reload).
 */
import type { Comment, CommentTargetType, Review } from '@komika/types';
import { backend } from '$lib/context';
import { config } from '$lib/config';
import { auth } from '$lib/auth.svelte';
import * as local from './social';
import type { ReaderComment, SeriesComment } from './types';

/** Live social requires the unified backend (Suwayomi/mock have no auth layer). */
export function socialLive(): boolean {
	return config.backendEnabled && config.backendKind === 'komika';
}

// ---- view shapes -----------------------------------------------------------

export interface ReviewView {
	id: string;
	name: string;
	initial: string;
	/** Author id — stable colour key for the fallback initial. */
	authorId?: string;
	/** Uploaded avatar URL (live backend only); undefined → render an initial. */
	avatarUrl?: string | null;
	score: number; // 1..10, 0 = none
	body: string;
	time: string;
	hasSpoiler: boolean;
	mine: boolean;
	likes: number;
	liked: boolean;
}

export interface CommentView {
	id: string;
	authorId: string;
	name: string;
	initial: string;
	/** Uploaded avatar URL (live backend only); undefined → render an initial. */
	avatarUrl?: string | null;
	bg: string;
	fg: string;
	body: string;
	time: string;
	hasSpoiler: boolean;
	isOp: boolean;
	mine: boolean;
	likes: number;
	liked: boolean;
}

export interface SeriesSocial {
	myScore: number;
	myBody: string;
	reviews: ReviewView[];
}

// ---- helpers ---------------------------------------------------------------

function initialOf(name: string): string {
	return (name.trim()[0] ?? '?').toUpperCase();
}

/** Deterministic avatar colour from a string (stable across reloads). */
const AVATARS: { bg: string; fg: string }[] = [
	{ bg: '#3a2f4a', fg: '#d9c7f0' },
	{ bg: '#2f4a3a', fg: '#a7e0c0' },
	{ bg: '#4a2f33', fg: '#f0b7bd' },
	{ bg: '#2f3a4a', fg: '#a7c8f0' },
	{ bg: '#4a412f', fg: '#f0dca7' },
	{ bg: '#2f4a48', fg: '#a7f0ea' },
];
function avatarFor(key: string): { bg: string; fg: string } {
	let h = 0;
	for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) >>> 0;
	return AVATARS[h % AVATARS.length];
}

function relTime(iso: string | null | undefined): string {
	if (!iso) return 'just now';
	const then = Date.parse(iso);
	if (Number.isNaN(then)) return 'just now';
	const now = typeof Date !== 'undefined' && Date.now ? Date.now() : then;
	const mins = Math.max(0, Math.round((now - then) / 60000));
	if (mins < 1) return 'just now';
	if (mins < 60) return `${mins}m ago`;
	const hrs = Math.round(mins / 60);
	if (hrs < 24) return `${hrs}h ago`;
	return `${Math.round(hrs / 24)}d ago`;
}

function reviewToView(r: Review): ReviewView {
	const mine = !!auth.user && r.author.id === auth.user.id;
	return {
		id: r.id,
		name: r.author.username,
		initial: initialOf(r.author.username),
		authorId: r.author.id,
		avatarUrl: r.author.avatarUrl,
		score: r.score,
		body: r.body,
		time: relTime(r.createdAt),
		hasSpoiler: r.hasSpoiler,
		mine,
		likes: 0,
		liked: false,
	};
}

function commentToView(c: Comment): CommentView {
	const av = avatarFor(c.author.id);
	return {
		id: c.id,
		authorId: c.author.id,
		name: c.author.username,
		initial: initialOf(c.author.username),
		avatarUrl: c.author.avatarUrl,
		bg: av.bg,
		fg: av.fg,
		body: c.body,
		time: relTime(c.createdAt),
		hasSpoiler: c.hasSpoiler,
		isOp: false,
		mine: !!auth.user && c.author.id === auth.user.id,
		likes: 0,
		liked: false,
	};
}

// ---- series: rating + reviews ----------------------------------------------

export async function loadSeriesSocial(seriesId: string, key: string): Promise<SeriesSocial> {
	if (socialLive()) {
		const { items } = await backend.reviews(seriesId);
		let views = items.map(reviewToView);
		// The paginated `reviews` list only returns page 1; on a busy series the
		// viewer's own (possibly early) review can fall off it, which would show
		// them as unrated with an empty body. Fetch their own review directly so
		// the widget always reflects it, and merge it in (prepend, or replace the
		// matching row if page 1 happened to include it).
		let mine = views.find((v) => v.mine);
		if (auth.user && backend.myReview) {
			const own = await backend.myReview(seriesId);
			if (own) {
				const ownView = reviewToView(own);
				views = [ownView, ...views.filter((v) => v.id !== ownView.id && !v.mine)];
				mine = ownView;
			}
		}
		return {
			myScore: mine?.score ?? 0,
			myBody: mine?.body ?? '',
			reviews: views.filter((v) => v.body.trim().length > 0),
		};
	}
	// Local fallback: no seeded thread (honest empty state) + a local per-series
	// rating. Any reviews the user posts offline persist in localStorage.
	const seeded = local.getComments('series', key, [] as SeriesComment[]);
	return {
		myScore: local.getRating('series', key),
		myBody: '',
		reviews: seeded.map((c) => ({
			id: String(c.id),
			name: c.name,
			initial: c.initial,
			score: 0,
			body: c.body,
			time: c.time,
			hasSpoiler: c.hasSpoiler ?? false,
			mine: false,
			likes: c.likes,
			liked: c.liked,
		})),
	};
}

/** Upsert the user's rating (score only; preserves any existing body). */
export async function saveSeriesRating(
	seriesId: string,
	key: string,
	score: number,
	body: string,
): Promise<void> {
	if (socialLive()) {
		if (score > 0) await backend.postReview({ seriesId, score, body, hasSpoiler: false });
		// score 0 (clear) has no backend delete; leave the prior rating in place.
		return;
	}
	local.setRating('series', key, score);
}

/** Post or update the user's written review (requires a score). */
export async function submitSeriesReview(
	seriesId: string,
	key: string,
	score: number,
	body: string,
	hasSpoiler: boolean,
	currentReviews: ReviewView[],
): Promise<ReviewView[]> {
	if (socialLive()) {
		// Defense-in-depth: the UI already gates these, but never post an
		// unauthenticated or score-less review even if a caller drops the gate.
		if (!auth.user) throw new Error('You must be signed in to post a review.');
		if (!(score > 0)) throw new Error('A review requires a score.');
		const r = await backend.postReview({ seriesId, score, body, hasSpoiler });
		const view = reviewToView(r);
		// Replace my existing review if present (upsert), else prepend.
		const without = currentReviews.filter((v) => !v.mine);
		return [view, ...without];
	}
	const next: ReviewView[] = [
		{
			id: 'local-' + currentReviews.length,
			name: 'You',
			initial: 'K',
			score,
			body,
			time: 'just now',
			hasSpoiler,
			mine: true,
			likes: 0,
			liked: false,
		},
		...currentReviews,
	];
	local.saveComments(
		'series',
		key,
		next.map((v) => ({
			id: Number(v.id.replace(/\D/g, '')) || Date.now(),
			name: v.name,
			initial: v.initial,
			chapter: '',
			time: v.time,
			body: v.body,
			hasSpoiler: v.hasSpoiler,
			likes: v.likes,
			liked: v.liked,
		})),
	);
	return next;
}

// ---- comments: per-chapter threads AND series-level discussion --------------
//
// One generalized pipeline serves both (mirrors the backend's polymorphic comment
// target). `LocalBucket` keeps the offline fallback for the two kinds in separate
// localStorage slots so a series discussion never collides with its reviews.

type LocalBucket = 'chapter' | 'seriesThread';

async function loadComments(
	targetType: CommentTargetType,
	targetId: string,
	bucket: LocalBucket,
	key: string,
): Promise<CommentView[]> {
	if (socialLive()) {
		const { items } = await backend.comments(targetType, targetId);
		// Server returns oldest→newest; the UI shows newest first.
		return items.map(commentToView).reverse();
	}
	// No seeded thread — honest empty state; offline user comments persist locally.
	const seeded = local.getComments(bucket, key, [] as ReaderComment[]);
	return seeded.map((c) => ({
		id: c.id,
		authorId: '',
		name: c.name,
		initial: c.initial,
		bg: c.bg,
		fg: c.fg,
		body: c.body,
		time: c.time,
		hasSpoiler: c.hasSpoiler ?? false,
		isOp: c.isOp,
		mine: false,
		likes: c.likes,
		liked: c.liked,
	}));
}

async function submitComment(
	targetType: CommentTargetType,
	targetId: string,
	bucket: LocalBucket,
	key: string,
	body: string,
	hasSpoiler: boolean,
	currentComments: CommentView[],
): Promise<CommentView[]> {
	if (socialLive()) {
		// Defense-in-depth: don't post a comment unauthenticated even if the UI
		// gate is bypassed.
		if (!auth.user) throw new Error('You must be signed in to comment.');
		const c = await backend.postComment({ targetType, targetId, body, hasSpoiler });
		return [commentToView(c), ...currentComments];
	}
	const me = auth.user?.username ?? 'You';
	const next: CommentView[] = [
		{
			id: 'local-' + Date.now(),
			authorId: auth.user?.id ?? '',
			name: me,
			initial: initialOf(me),
			bg: '#f2f1ee',
			fg: '#0c0c0d',
			body,
			time: 'just now',
			hasSpoiler,
			isOp: false,
			mine: true,
			likes: 0,
			liked: false,
		},
		...currentComments,
	];
	local.saveComments(
		bucket,
		key,
		next.map((c) => ({
			id: c.id,
			name: c.name,
			initial: c.initial,
			bg: c.bg,
			fg: c.fg,
			ts: 999,
			time: c.time,
			isOp: c.isOp,
			hasSpoiler: c.hasSpoiler,
			likes: c.likes,
			liked: c.liked,
			replies: 0,
			body: c.body,
		})),
	);
	return next;
}

// Per-chapter thread (reader page).
export function loadChapterComments(chapterId: string, key: string): Promise<CommentView[]> {
	return loadComments('chapter', chapterId, 'chapter', key);
}
export function submitChapterComment(
	chapterId: string,
	key: string,
	body: string,
	hasSpoiler: boolean,
	currentComments: CommentView[],
): Promise<CommentView[]> {
	return submitComment('chapter', chapterId, 'chapter', key, body, hasSpoiler, currentComments);
}

// Series-level discussion (comic detail page) — distinct from Reviews.
export function loadSeriesComments(seriesId: string, key: string): Promise<CommentView[]> {
	return loadComments('series', seriesId, 'seriesThread', key);
}
export function submitSeriesComment(
	seriesId: string,
	key: string,
	body: string,
	hasSpoiler: boolean,
	currentComments: CommentView[],
): Promise<CommentView[]> {
	return submitComment('series', seriesId, 'seriesThread', key, body, hasSpoiler, currentComments);
}

// ---- admin moderation ------------------------------------------------------

/** True when the signed-in user may moderate (admin on the live backend). */
export function canModerate(): boolean {
	return socialLive() && !!auth.user?.isAdmin;
}

/**
 * Delete a comment (admin moderation) and return the list without it. In live
 * mode this removes it server-side; the local fallback just drops it from view.
 */
export async function deleteChapterComment(
	commentId: string,
	current: CommentView[],
): Promise<CommentView[]> {
	if (socialLive()) {
		if (!auth.user?.isAdmin) throw new Error('Admin access required.');
		if (!backend.deleteComment) throw new Error('Moderation is unavailable on this backend.');
		await backend.deleteComment(commentId);
	}
	return current.filter((c) => c.id !== commentId);
}

/**
 * Ban a user (admin moderation). The server now hides banned users' comments
 * and reviews from reads, and they can no longer sign in.
 */
export async function banCommenter(userId: string): Promise<void> {
	if (!socialLive()) throw new Error('Banning requires the live backend.');
	if (!auth.user?.isAdmin) throw new Error('Admin access required.');
	if (!backend.banUser) throw new Error('Moderation is unavailable on this backend.');
	if (!userId) throw new Error('This comment has no account to ban.');
	await backend.banUser(userId, true);
}
