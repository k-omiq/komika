// Komika reader is a pure client-side SPA:
//  - required by Tauri (desktop + mobile) which serves static assets, and
//  - keeps the web build server-less (no SSR runtime = no server attack surface).
export const ssr = false;
export const prerender = false;
