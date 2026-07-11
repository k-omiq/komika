import type { Page } from '@komika/types';
import { isTauri } from './platform.js';

/**
 * Turns an upstream source image URL into something the UI can put in an
 * <img src>. This is the seam that hides Komika's web/native split from every
 * component: the reader just asks the provider to resolve a page.
 *
 *  - web    → returns a Cloudflare Worker proxy URL (bypasses CORS; the Worker
 *             fetches upstream bytes and edge-caches them).
 *  - native → invokes a Rust command that fetches the bytes directly from the
 *             source CDN and hands back a displayable URL (blob / asset proto).
 */
export interface ImageProvider {
	/** Resolve a page image to a displayable URL. */
	resolvePage(page: Page): Promise<string>;
	/** Resolve a cover / thumbnail URL (same rules as pages). */
	resolveCover(sourceUrl: string): Promise<string>;
	/** Release any object URLs created for a page (native blob URLs leak otherwise). */
	release?(url: string): void;
}

export interface WebImageProviderConfig {
	/** Base URL of the Cloudflare Worker image proxy, e.g. https://img.komika.app */
	workerBaseUrl: string;
	/**
	 * When true, return source URLs unchanged instead of proxying them. Use when
	 * the images are already same-origin / CORS-safe (e.g. a Suwayomi server that
	 * proxies source images itself), so no Worker is needed.
	 */
	direct?: boolean;
}

/** Web provider: rewrite source URLs to Worker-proxied URLs (or pass through in direct mode). */
export class WebImageProvider implements ImageProvider {
	constructor(private readonly config: WebImageProviderConfig) {}

	private resolve(sourceUrl: string): string {
		if (!sourceUrl) return '';
		if (this.config.direct) return sourceUrl;
		const base = this.config.workerBaseUrl.replace(/\/$/, '');
		return `${base}/img?src=${encodeURIComponent(sourceUrl)}`;
	}

	async resolvePage(page: Page): Promise<string> {
		return this.resolve(page.sourceUrl);
	}

	async resolveCover(sourceUrl: string): Promise<string> {
		return this.resolve(sourceUrl);
	}
}

/**
 * Native provider: fetch bytes through the Rust core and expose them as a blob
 * URL. The actual `invoke("fetch_image", …)` call is wired when the Tauri app
 * is generated; kept dynamic so this package has no hard Tauri dependency and
 * still builds for the web target.
 */
export class NativeImageProvider implements ImageProvider {
	private async fetchBytes(sourceUrl: string): Promise<ArrayBuffer> {
		const { invoke } = await import('@tauri-apps/api/core');
		// Rust `fetch_image` returns the raw image bytes as an ArrayBuffer.
		return invoke<ArrayBuffer>('fetch_image', { url: sourceUrl });
	}

	private async toBlobUrl(sourceUrl: string): Promise<string> {
		const bytes = await this.fetchBytes(sourceUrl);
		const blob = new Blob([bytes]);
		return URL.createObjectURL(blob);
	}

	async resolvePage(page: Page): Promise<string> {
		return this.toBlobUrl(page.sourceUrl);
	}

	async resolveCover(sourceUrl: string): Promise<string> {
		return this.toBlobUrl(sourceUrl);
	}

	release(url: string): void {
		if (url.startsWith('blob:')) URL.revokeObjectURL(url);
	}
}

export function createImageProvider(config: WebImageProviderConfig): ImageProvider {
	return isTauri() ? new NativeImageProvider() : new WebImageProvider(config);
}
