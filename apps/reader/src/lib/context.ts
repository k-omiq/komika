import {
	createBackend,
	createCompositeBackend,
	createLocalStorageAdapter,
	createLocalSuwayomiBackend,
	createSuwayomiBackend,
	createImageProvider,
	currentPlatform,
	isTauri,
	OfflineWriteQueue,
} from '@komika/api';
import { config } from './config';

/**
 * App-wide singletons. The UI only ever talks to these two seams:
 *  - `backend` — the data backend (catalog, library, social, auth). The unified
 *                Komika API, a direct Suwayomi adapter, or — when the native
 *                engine flag is on inside Tauri — a CompositeBackend that can
 *                serve content on-device while delegating everything else to the
 *                hosted API. Selected per config.
 *  - `images`  — resolves source image URLs; automatically Worker-proxied on web
 *                and Rust-fetched-direct on native (Tauri). Components never know
 *                which platform they're on.
 */
const komikaHosted = createBackend({ endpoint: config.apiEndpoint });
export const backend =
	config.backendKind === 'suwayomi'
		? createSuwayomiBackend({ baseUrl: config.suwayomiUrl })
		: config.nativeEngine && isTauri()
			? createCompositeBackend({
					hosted: komikaHosted,
					local: createLocalSuwayomiBackend(),
					// Durable offline write-queue (native only; plan §9): captures failed
					// `mark`/`setProgress` writes and replays them when connectivity returns.
					queue: new OfflineWriteQueue(createLocalStorageAdapter('komika.offlineWrites')),
				})
			: komikaHosted;
export const images = createImageProvider({
	workerBaseUrl: config.imgWorkerBaseUrl,
	direct: config.imgDirect,
});
export const platform = currentPlatform();
