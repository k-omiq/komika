import type { PageLoad } from './$types';
import { getUpdates } from '$lib/data/source';

// Stream the updates feeds so the page can show skeletons while they resolve.
// `getUpdates()` never rejects — it falls back to mock on error.
export const load: PageLoad = () => ({ updates: getUpdates() });
