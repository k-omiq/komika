# Komika

Hosted manga/comics reader — "Mihon as a service, with a social layer." Svelte +
Tauri v2, one codebase for **web, desktop, and iOS/Android**. See
[`SPEC.md`](./SPEC.md) for the full architecture.

## Prerequisites

- Node ≥ 20, **pnpm** (`corepack enable`)
- Rust (stable) — for the desktop/mobile shell
- Xcode (iOS), Android SDK/NDK (Android)

## Setup

```sh
pnpm install
```

## Develop

```sh
pnpm dev:reader          # reader app (web) at http://localhost:5173
pnpm dev:admin           # admin console at http://localhost:5273

# desktop app (Rust + webview)
pnpm --filter @komika/reader desktop:dev
```

## Build

```sh
pnpm build               # all packages + apps (static SPAs)
pnpm --filter @komika/reader desktop:build   # desktop binaries
```

## Workspace

| Package          | What                                             |
| ---------------- | ------------------------------------------------ |
| `@komika/reader` | user app (SvelteKit SPA + `src-tauri` Rust core) |
| `@komika/admin`  | admin "manga DB" console (web-only SPA)          |
| `@komika/types`  | shared domain types                              |
| `@komika/api`    | data layer — `Backend` + `ImageProvider`         |
| `@komika/ui`     | design tokens + shared components                |

## Status

Foundation only. Every **screen is a placeholder** until the Claude Design
project can be read into the workspace — see `SPEC.md` → "Blocked: design access".
