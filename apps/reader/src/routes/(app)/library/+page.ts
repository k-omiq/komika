import type { PageLoad } from './$types';
import { getLibrary } from '$lib/data/source';

// Resolve the library in `load` (this is a static SPA — `ssr = false`) so the page
// renders with a ready value, mirroring the profile page. `getLibrary()` never
// rejects — it resolves to an empty library on error.
export const load: PageLoad = async () => ({ library: await getLibrary() });
