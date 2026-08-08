#!/bin/bash
set -euo pipefail

# OAB ECS Entrypoint Wrapper
# Downloads bootstrap archive and rendered config before starting OAB.

# 1. Restore bootstrap (mutable state: memory, knowledge base)
if [ -n "${BOOTSTRAP_FROM:-}" ]; then
    echo "[entrypoint] Restoring bootstrap from ${BOOTSTRAP_FROM}..."
    aws s3 cp "${BOOTSTRAP_FROM}" /tmp/bootstrap.tar.gz
    tar xzf /tmp/bootstrap.tar.gz -C "$HOME"
    rm -f /tmp/bootstrap.tar.gz
fi

# 2. Overwrite with rendered config (AFTER bootstrap, so desired config wins)
if [ -n "${CONFIG_S3_PATH:-}" ]; then
    echo "[entrypoint] Downloading config from ${CONFIG_S3_PATH}..."
    aws s3 cp "${CONFIG_S3_PATH}" "$HOME/config.toml"
fi

# 3. Start OAB (DISCORD_TOKEN etc injected via ECS secrets)
echo "[entrypoint] Starting OpenAB..."
exec /usr/bin/openab "$@"
