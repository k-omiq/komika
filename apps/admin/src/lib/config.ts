import { env } from '$env/dynamic/public';

/**
 * Admin console config. The console only makes sense against the unified Komika
 * API (it needs the admin mutations + auth), so it always targets that endpoint.
 */
export const config = {
	apiEndpoint: env.PUBLIC_KOMIKA_API ?? 'http://localhost:8080/graphql',
};
