#!/usr/bin/env bash
# Build the pinned upstream gstack browser in the disposable CI runner.
set -euo pipefail

GSTACK_REF=e66c43c57209a17e2d9a79847a2e985c27ca80bd
GSTACK_DIR="${RUNNER_TEMP:?RUNNER_TEMP is required}/campfire-gstack"

git init --quiet "$GSTACK_DIR"
git -C "$GSTACK_DIR" remote add origin https://github.com/garrytan/gstack.git
git -C "$GSTACK_DIR" fetch --depth 1 origin "$GSTACK_REF"
git -C "$GSTACK_DIR" checkout --quiet --detach FETCH_HEAD
test "$(git -C "$GSTACK_DIR" rev-parse HEAD)" = "$GSTACK_REF"

cd "$GSTACK_DIR"
bun install --frozen-lockfile
bun run build
./node_modules/.bin/playwright install --with-deps chromium
test -x "$GSTACK_DIR/browse/dist/browse"

printf 'BROWSE_BIN=%s\n' "$GSTACK_DIR/browse/dist/browse" >> "${GITHUB_ENV:?GITHUB_ENV is required}"
printf 'GSTACK_REF=%s\n' "$GSTACK_REF" >> "$GITHUB_ENV"
printf 'gstack_commit=%s\n' "$GSTACK_REF"
printf 'bun_version=%s\n' "$(bun --version)"
printf 'playwright_version=%s\n' "$(./node_modules/.bin/playwright --version)"
sha256sum "$GSTACK_DIR/browse/dist/browse"
