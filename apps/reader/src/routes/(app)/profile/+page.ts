import type { PageLoad } from './$types';
import { getProfile } from '$lib/data/source';

export const load: PageLoad = async () => ({ profile: await getProfile() });
