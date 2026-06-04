import type { PageLoad } from './$types';
import { getHome } from '$lib/data/source';

// Stream the home feeds: return the promise (unawaited) so the page can render
// skeleton placeholders via {#await} while the backend resolves. `getHome()`
// never rejects — it resolves to empty feeds on error.
export const load: PageLoad = () => ({ home: getHome() });
