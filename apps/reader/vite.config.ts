import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// Tauri sets this when developing against a physical mobile device.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true,
			},
			// Static SPA output: required for Tauri (desktop + mobile) and for the
			// hardened, server-less web/PWA build. `fallback` makes it a true SPA so
			// client-side routing resolves for every path.
			adapter: adapter({ fallback: 'index.html' }),
		}),
	],
	// --- Tauri-friendly dev server ---
	clearScreen: false,
	server: {
		port: 5173,
		strictPort: true,
		host: host || false,
		hmr: host ? { protocol: 'ws', host, port: 5174 } : undefined,
		watch: {
			// Vite shouldn't watch the Rust side.
			ignored: ['**/src-tauri/**'],
		},
	},
});
