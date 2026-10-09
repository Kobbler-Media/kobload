#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_dev="${root_dir}/docker-compose.dev.yml"
compose_args=(-f "${compose_dev}")

reset_flag="${KOBLOAD_RESET:-0}"

nats_compose="${root_dir}/../koblibs/docker-compose.nats.yml"

if [[ "${reset_flag}" == "1" ]]; then
  echo "Resetting volumes..."
  docker compose "${compose_args[@]}" down -v
  # Also reset shared NATS data to avoid stale consumer state
  if [[ -f "${nats_compose}" ]]; then
    echo "Resetting NATS volumes..."
    docker compose -f "${nats_compose}" down -v
  fi
fi

# Start shared NATS (creates kob_dev network if needed)
if [[ -f "${nats_compose}" ]]; then
  echo "Starting shared NATS..."
  docker compose -f "${nats_compose}" up -d
else
  echo "Warning: NATS compose file not found at ${nats_compose}"
  echo "Creating kob_dev network if needed..."
  docker network create kob_dev 2>/dev/null || true
fi

export KOBLOAD_PORT="${KOBLOAD_PORT:-7080}"
export KOBLOAD_WEB_PORT="${KOBLOAD_WEB_PORT:-5174}"

echo "Starting Kobload (dev)..."
docker compose "${compose_args[@]}" up -d --build

api_id=""
for _ in $(seq 1 20); do
  api_id="$(docker compose "${compose_args[@]}" ps -q api | tail -n 1)"
  if [[ -n "${api_id}" ]]; then
    break
  fi
  sleep 1
done

if [[ -z "${api_id}" ]]; then
  echo "API container not found." >&2
  exit 1
fi

echo "Waiting for API health..."
healthy=0
base_url="http://localhost:${KOBLOAD_PORT}"
for _ in $(seq 1 60); do
  if curl -fsS "${base_url}/health" >/dev/null 2>&1; then
    healthy=1
    break
  fi
  sleep 1
done

if [[ "${healthy}" -ne 1 ]]; then
  echo "API health check failed at ${base_url}/health" >&2
  exit 1
fi

admin_password=""
for _ in $(seq 1 30); do
  log_line="$(docker logs "${api_id}" 2>/dev/null | sed -r 's/\\x1b\\[[0-9;]*m//g' | grep -oP "password=[^ ]+" | head -1 || true)"
  if [[ -n "${log_line}" ]]; then
    admin_password="${log_line#password=}"
    break
  fi
  sleep 1
done

if [[ -n "${admin_password}" ]]; then
  echo "Login details:"
  echo "- Admin email: admin@localhost"
  echo "- Admin password: ${admin_password}"
else
  echo "Admin password not found in logs (was this an existing volume or using .env.dev?)."
fi

echo "UI URL: http://localhost:${KOBLOAD_WEB_PORT}"
echo "API URL: ${base_url}"
