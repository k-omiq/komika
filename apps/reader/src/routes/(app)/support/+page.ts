import type { PageLoad } from './$types';
import { getSupport } from '$lib/data/source';

export const load: PageLoad = () => getSupport();
