import type { PageLoad } from './$types';
import { getDonate } from '$lib/data/source';

export const load: PageLoad = () => getDonate();
