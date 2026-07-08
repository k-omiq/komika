import type { PageLoad } from './$types';
import { getHome } from '$lib/data/source';

// Stream the home feeds: return the promise (unawaited) so the page can render
// skeleton placeholders via {#await} while the backend (or mock fallback)
// resolves. `getHome()` never rejects — it falls back to mock on error.
export const load: PageLoad = () => ({ home: getHome() });
