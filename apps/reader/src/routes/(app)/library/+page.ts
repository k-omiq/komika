import type { PageLoad } from './$types';
import { getLibrary } from '$lib/data/source';

// Stream the library so the grid can show a skeleton while it resolves.
// `getLibrary()` never rejects — it resolves to an empty library on error.
export const load: PageLoad = () => ({ library: getLibrary() });
