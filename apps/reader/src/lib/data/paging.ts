/**
 * Shared page-number plumbing for the reader's server-paginated feeds (Browse,
 * and Updates once its server feed lands).
 *
 * The reader's pagers put the page number in the URL rather than in component
 * state (which is what the admin console does): `/browse` is public, edge-SSR'd
 * and shareable, so a page has to survive a reload, a copy-paste and the
 * browser's Back button. Everything here exists to make that one param safe.
 */

/**
 * Rows per page. Mirrors the server's `PAGE_SIZE`
 * (`apps/server/src/graphql/mod.rs`) — the reader has no way to ask for it, so
 * it is duplicated here, the same way `apps/admin/src/routes/updates/+page.svelte`
 * duplicates it. Only used for range/last-page arithmetic; the server decides
 * the real window, so a drift here mis-labels ranges but never mis-pages.
 */
export const FEED_PAGE_SIZE = 20;

/**
 * The 1-based page from a URL search param.
 *
 * Anything non-numeric, `< 1`, non-integer or absurdly large collapses to 1: a
 * hand-edited or truncated shared link must never turn into `OFFSET NaN` or a
 * ten-million-row offset scan on the server. `max` is a sanity ceiling, not a
 * real bound — over-range pages inside it are handled by the caller (they land
 * on the last real page once `total` is known).
 */
export function pageParam(sp: URLSearchParams, max = 100_000): number {
	const raw = sp.get('page');
	if (raw == null || raw.trim() === '') return 1;
	const n = Number(raw);
	return Number.isInteger(n) && n >= 1 && n <= max ? n : 1;
}

/**
 * The current URL with `page` set to `p`, as a root-relative href.
 *
 * `page=1` is OMITTED rather than written: `/browse` and `/browse?page=1` are
 * the same content, and emitting both would give them two edge-cache keys, two
 * crawlable URLs and two back-nav cache entries. Every other param is carried
 * through untouched, so a pager step never disturbs the filters in the URL.
 */
export function withPage(url: URL, p: number): string {
	const sp = new URLSearchParams(url.searchParams);
	if (p > 1) sp.set('page', String(p));
	else sp.delete('page');
	const qs = sp.toString();
	return qs ? `${url.pathname}?${qs}` : url.pathname;
}

/** The last page that can hold a row, given a known total. Never below 1. */
export function lastPage(total: number, pageSize = FEED_PAGE_SIZE): number {
	return Math.max(1, Math.ceil(total / pageSize));
}
