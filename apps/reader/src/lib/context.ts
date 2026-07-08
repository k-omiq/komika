import {
	createBackend,
	createSuwayomiBackend,
	createImageProvider,
	currentPlatform,
} from '@komika/api';
import { config } from './config';

/**
 * App-wide singletons. The UI only ever talks to these two seams:
 *  - `backend` — the data backend (catalog, library, social, auth). Either the
 *                unified Komika API or a direct Suwayomi adapter, per config.
 *  - `images`  — resolves source image URLs; automatically Worker-proxied on web
 *                and Rust-fetched-direct on native (Tauri). Components never know
 *                which platform they're on.
 */
export const backend =
	config.backendKind === 'suwayomi'
		? createSuwayomiBackend({ baseUrl: config.suwayomiUrl })
		: createBackend({ endpoint: config.apiEndpoint });
export const images = createImageProvider({
	workerBaseUrl: config.imgWorkerBaseUrl,
	direct: config.imgDirect,
});
export const platform = currentPlatform();
