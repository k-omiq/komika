/**
 * Admin data layer — catalog listing + override writes, straight through the
 * unified backend (no mock fallback: the console requires the real API).
 */
import type { Series, SeriesStatus } from '@komika/types';
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

export const STATUS_OPTIONS: SeriesStatus[] = [
	'ONGOING',
	'COMPLETED',
	'HIATUS',
	'CANCELLED',
	'UNKNOWN',
];
