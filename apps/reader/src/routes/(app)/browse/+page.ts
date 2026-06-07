import type { PageLoad } from './$types';
import { getGenreFacets } from '$lib/data/source';

// Load the genre facet set for the filter multi-select (S4). Row fetching happens
// client-side in the page (reactive to query + filters); `getGenreFacets()` never
// rejects — it resolves to an empty list on error.
export const load: PageLoad = () => ({ facets: getGenreFacets() });
