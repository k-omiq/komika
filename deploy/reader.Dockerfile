# Multi-stage build for the reader SPA (apps/reader), served by nginx.
# Build context is the REPO ROOT:
#   docker build -f deploy/reader.Dockerfile -t komika-reader .
#
# adapter-static emits a fully static SPA to apps/reader/build with an
# index.html fallback, so any tiny static server works. nginx is used here for
# the SPA rewrite + long-cache immutable assets.
#
# Build-time public config is baked in (Vite inlines PUBLIC_* at build time).
# Point the reader at the backend by passing build args, e.g.:
#   --build-arg PUBLIC_KOMIKA_API=https://api.example.com/graphql

# ---- builder ----------------------------------------------------------------
FROM node:22-slim AS builder
WORKDIR /repo

# Enable the pinned pnpm via corepack.
RUN corepack enable && corepack prepare pnpm@11.9.0 --activate

# Copy the whole workspace: the reader depends on packages/* via workspace:*,
# so a partial copy would break resolution. Lockfile is at the root.
COPY . .

RUN pnpm install --frozen-lockfile

# Reader backend wiring (see apps/server/README.md "Point the reader at it").
# Defaults keep the local mock fallback; override for a real deploy.
ARG PUBLIC_KOMIKA_BACKEND=off
ARG PUBLIC_KOMIKA_BACKEND_KIND=komika
ARG PUBLIC_KOMIKA_API=http://localhost:8080/graphql
ARG PUBLIC_KOMIKA_IMG_MODE=direct
ENV PUBLIC_KOMIKA_BACKEND=$PUBLIC_KOMIKA_BACKEND \
    PUBLIC_KOMIKA_BACKEND_KIND=$PUBLIC_KOMIKA_BACKEND_KIND \
    PUBLIC_KOMIKA_API=$PUBLIC_KOMIKA_API \
    PUBLIC_KOMIKA_IMG_MODE=$PUBLIC_KOMIKA_IMG_MODE

RUN pnpm --filter @komika/reader build

# ---- runtime ----------------------------------------------------------------
FROM nginx:1.27-alpine AS runtime

# SPA config: history-fallback to index.html, cache immutable hashed assets.
COPY deploy/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /repo/apps/reader/build /usr/share/nginx/html

EXPOSE 80

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -qO- http://localhost/ >/dev/null 2>&1 || exit 1
