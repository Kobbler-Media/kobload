#!/bin/sh
set -e

# Ensure node_modules is in sync with package.json
# This is needed because we use a named volume for node_modules
# which persists between builds and may have stale dependencies

if [ -f /app/package.json ]; then
    echo "Checking dependencies..."
    npm install --prefer-offline --no-audit --package-lock=false 2>/dev/null || npm install --package-lock=false
fi

exec "$@"
