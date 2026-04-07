#!/usr/bin/env bash
#
# Komika — one-command deploy.
#
# Stands up the FULL public stack (Suwayomi + FlareSolverr + Komika server +
# reader), then bootstraps it into a working "hosted Mihon/Tachiyomi with a public
# social layer and an auto-updating catalog": installs the MangaDex source, turns
# on the Cloudflare bypass, creates the admin, and seeds the library.
#
#   ./deploy.sh            build + start + bootstrap  (default)
#   ./deploy.sh up         same as default
#   ./deploy.sh bootstrap  (re)run the bootstrap only (stack already up)
#   ./deploy.sh down       stop the stack (keeps data volumes)
#   ./deploy.sh destroy    stop and DELETE the data volumes
#   ./deploy.sh logs       tail all service logs
#
set -euo pipefail
cd "$(dirname "$0")"

cy='\033[36m'; gn='\033[32m'; yl='\033[33m'; rd='\033[31m'; bd='\033[1m'; z='\033[0m'
say(){ printf "${cy}▸ %s${z}\n" "$*"; }
die(){ printf "${rd}✗ %s${z}\n" "$*" >&2; exit 1; }

# ---- prerequisites ------------------------------------------------------------
command -v docker >/dev/null 2>&1 || die "docker is not installed — install Docker first."
if docker compose version >/dev/null 2>&1; then DC="docker compose";
elif command -v docker-compose >/dev/null 2>&1; then DC="docker-compose";
else die "docker compose is not available."; fi
command -v python3 >/dev/null 2>&1 || die "python3 is required to run the bootstrap."

# ---- config -------------------------------------------------------------------
if [ ! -f .env ]; then
  cp .env.example .env
  printf "${yl}! created deploy/.env from the template.${z}\n"
  die "review deploy/.env before deploying — set a real KOMIKA_ADMIN_PASSWORD (the template ships a public placeholder) and PUBLIC_HOST, then re-run ./deploy.sh."
fi
set -a; . ./.env 2>/dev/null || true; set +a
PUBLIC_HOST="${PUBLIC_HOST:-localhost}"
READER_PORT="${READER_PORT:-8081}"; SERVER_PORT="${SERVER_PORT:-8080}"; SUWAYOMI_PORT="${SUWAYOMI_PORT:-4567}"

cmd="${1:-up}"
case "$cmd" in
  down)    say "stopping the stack (data volumes kept)"; $DC down; exit 0 ;;
  destroy) say "stopping the stack and DELETING data volumes"; $DC down -v; exit 0 ;;
  logs)    exec $DC logs -f ;;
  bootstrap) python3 bootstrap.py; exit $? ;;
  up|"") : ;;
  *) die "unknown command '$cmd' (use: up | bootstrap | down | destroy | logs)" ;;
esac

# ---- build + start ------------------------------------------------------------
say "building images and starting services (this can take a few minutes the first time)…"
$DC up --build -d

say "waiting for services to report healthy…"
# Give the stack up to ~5 min to go healthy (Suwayomi's first boot is slow).
# Track whether we broke out because everything went healthy vs. timed out, so a
# persistently-unhealthy stack fails loudly instead of bootstrapping half-up.
healthy=""
for _ in $(seq 1 100); do
  unhealthy=$($DC ps --format '{{.Service}} {{.Health}}' 2>/dev/null | awk '$2!="" && $2!="healthy"{print $1}' | tr '\n' ' ')
  if [ -z "${unhealthy// }" ]; then healthy=1; break; fi
  sleep 3
done
# Timed out with services still not healthy → don't bootstrap a partial stack.
[ -n "$healthy" ] || die "services did not become healthy: ${unhealthy% }"

# ---- bootstrap ----------------------------------------------------------------
say "bootstrapping (extensions, Cloudflare bypass, admin, library seed)…"
python3 bootstrap.py

# ---- done ---------------------------------------------------------------------
printf "\n${gn}${bd}Komika is up.${z}\n"
printf "  ${bd}Reader${z}      http://%s:%s\n" "$PUBLIC_HOST" "$READER_PORT"
printf "  ${bd}API${z}         http://%s:%s/graphql\n" "$PUBLIC_HOST" "$SERVER_PORT"
printf "  ${bd}Suwayomi${z}    http://%s:%s   (source management / images)\n" "$PUBLIC_HOST" "$SUWAYOMI_PORT"
printf "  admin login: ${bd}%s${z} / (KOMIKA_ADMIN_PASSWORD in deploy/.env)\n" "${KOMIKA_ADMIN_USERS%%,*}"
printf "\n  Manage:  ./deploy.sh logs   ·   ./deploy.sh down   ·   ./deploy.sh destroy\n"
printf "  The adaptive scanner auto-updates the seeded library — no manual refresh needed.\n"

# ---- backup status ------------------------------------------------------------
# Continuous backup is opt-in: server-entrypoint.sh only runs the DB under
# Litestream when ALL of LITESTREAM_BUCKET / LITESTREAM_ACCESS_KEY_ID /
# LITESTREAM_SECRET_ACCESS_KEY are set (mirror that exact gate here). If backup
# is off, the single data volume holds accounts, argon2 hashes, session tokens,
# reviews, comments and scan-state with NO copy — warn loudly so it isn't a
# silent data-loss surprise.
if [ -n "${LITESTREAM_BUCKET:-}" ] && [ -n "${LITESTREAM_ACCESS_KEY_ID:-}" ] && [ -n "${LITESTREAM_SECRET_ACCESS_KEY:-}" ]; then
  printf "\n${gn}✓ Continuous backup ENABLED${z} — DB replicated to %s via Litestream.\n" "${LITESTREAM_BUCKET}"
else
  printf "\n${rd}${bd}⚠ NO BACKUP CONFIGURED${z}${rd} — the database has NO continuous backup.${z}\n"
  printf "${rd}  A volume loss or ${bd}./deploy.sh destroy${z}${rd} is UNRECOVERABLE: accounts, password\n"
  printf "  hashes, session tokens, reviews, comments and scan-state would be gone.${z}\n"
  printf "${rd}  Enable it by setting the ${bd}LITESTREAM_*${z}${rd} vars in ${bd}deploy/.env${z}${rd} (see deploy/.env.example).${z}\n"
fi
