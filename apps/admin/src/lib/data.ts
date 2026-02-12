/**
 * Admin data layer — catalog listing + override writes, straight through the
 * unified backend (no mock fallback: the console requires the real API).
 */
import type {
	AdminUser,
	CanonicalUpdate,
	MergeCandidate,
	Paginated,
	Series,
	SeriesStatus,
} from '@komika/types';
import type { SeriesAdminInput } from '@komika/api';
import { backend } from './context';

/**
 * The catalog the console manages: a search when a query is given, otherwise the
 * curated library (falling back to popular so the console isn't empty on a fresh
 * install).
 */
export async function loadCatalog(query: string): Promise<Series[]> {
	const q = query.trim();
	if (q) {
		const { items } = await backend.search(q);
		return items;
	}
	const lib = await backend.library();
	if (lib.length) return lib;
	const { items } = await backend.search('');
	return items;
}

/** Persist per-series overrides (whole-state) and return the recomputed series. */
export async function saveSeriesAdmin(input: SeriesAdminInput): Promise<Series> {
	if (!backend.updateSeriesAdmin) throw new Error('Admin API is unavailable on this backend.');
	return backend.updateSeriesAdmin(input);
}

/** Force an immediate re-scan of one series and return its refreshed state. */
export async function triggerScan(seriesId: string): Promise<Series> {
	if (!backend.triggerScan) throw new Error('Manual scan is unavailable on this backend.');
	return backend.triggerScan(seriesId);
}

/** Paginated user list for the user-management console. */
export async function loadUsers(page = 1): Promise<Paginated<AdminUser>> {
	if (!backend.users) throw new Error('User management is unavailable on this backend.');
	return backend.users(page);
}

/** Suspend or restore a user account. */
export async function setUserBanned(userId: string, banned: boolean): Promise<void> {
	if (!backend.banUser) throw new Error('User management is unavailable on this backend.');
	await backend.banUser(userId, banned);
}

/** Grant or revoke a user's admin flag; returns the updated user. */
export async function setUserAdmin(userId: string, isAdmin: boolean): Promise<AdminUser> {
	if (!backend.setUserAdmin) throw new Error('User management is unavailable on this backend.');
	return backend.setUserAdmin(userId, isAdmin);
}

/** Pending dedup matches awaiting manual review (CATALOGUE.md §4). */
export async function loadMergeQueue(): Promise<MergeCandidate[]> {
	if (!backend.mergeQueue) throw new Error('Dedup review is unavailable on this backend.');
	return backend.mergeQueue();
}

/**
 * Resolve a dedup review. `accept` merges the source series into the candidate
 * work; rejecting keeps it as a distinct first-class work. Returns true when the
 * row was closed.
 */
export async function resolveMergeCandidate(id: string, accept: boolean): Promise<boolean> {
	if (!backend.resolveMergeCandidate)
		throw new Error('Dedup review is unavailable on this backend.');
	return backend.resolveMergeCandidate(id, accept);
}

/**
 * Recently-updated mirrored MangaDex works + their latest stored chapter, from the
 * canonical `chapter` mirror (CATALOGUE.md §6). A monitoring feed for the mirror —
 * these works are not reader-openable yet.
 */
export async function loadCanonicalUpdates(page = 1): Promise<CanonicalUpdate[]> {
	if (!backend.canonicalUpdates)
		throw new Error('Catalogue updates are unavailable on this backend.');
	return backend.canonicalUpdates(page);
}

export const STATUS_OPTIONS: SeriesStatus[] = [
	'ONGOING',
	'COMPLETED',
	'HIATUS',
	'CANCELLED',
	'UNKNOWN',
];
