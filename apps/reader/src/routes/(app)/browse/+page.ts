import type { PageLoad } from './$types';
import { getBrowseCatalog } from '$lib/data/source';

// Stream the catalog so the page can show a grid skeleton while it resolves.
// `getBrowseCatalog()` never rejects — it resolves to an empty catalog on error.
export const load: PageLoad = () => ({ catalog: getBrowseCatalog() });
