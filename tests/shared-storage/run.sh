#!/usr/bin/env bash
# Copyright 2026 Salesforce, Inc. All rights reserved.
#
# Shared-storage integration test: two Flex replicas, two API instances, one Redis.
# Usage: [REGISTRATION=<path>] tests/shared-storage/run.sh   (after `make build`)
#    or: [REGISTRATION=<path>] make test-shared-storage
# SKIP_SDK=1 skips the MCP SDK checks (sdk_check.py, ts_check.mjs), which install from PyPI and npm.
# Flex needs a registration even in local mode. REGISTRATION defaults to the playground's
# (gitignored) playground/config/registration.yaml; the file is mounted read-only, never copied.
set -euo pipefail

cd "$(dirname "$0")"
ROOT=../..
REGISTRATION=${REGISTRATION:-$ROOT/playground/config/registration.yaml}
if [ ! -f "$REGISTRATION" ]; then
  echo "No Flex registration at $REGISTRATION. Set REGISTRATION=<path to registration.yaml>." >&2
  exit 1
fi
export REGISTRATION=$(cd "$(dirname "$REGISTRATION")" && pwd)/$(basename "$REGISTRATION")
RELEASE="$ROOT/target/wasm32-wasip1/release"
POLICY_REF=$(cat "$ROOT/target/policy-ref-name.txt")

rm -rf config/custom-policies && mkdir -p config/custom-policies
cp "$RELEASE"/*_definition.yaml "$RELEASE"/*_implementation.yaml config/custom-policies/
sed "s/POLICY_REF/$POLICY_REF/" api.yaml.in > config/api.yaml

cleanup() {
  if [ "${KEEP:-}" = "" ]; then docker compose down -v >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT

docker compose down -v >/dev/null 2>&1 || true
docker compose up -d

# The test must never join a real Anypoint environment: stop unless Flex runs in local mode.
for _ in $(seq 60); do
  mode=$(docker compose logs flex-1 2>/dev/null | grep -o 'Mode=[a-z]*' | head -1 || true)
  [ -n "$mode" ] && break
  sleep 2
done
if [ -z "$mode" ] || [ "$mode" = "Mode=connected" ]; then
  echo "Flex is not in local mode (${mode:-mode unknown}). Use a registration created with --connected=false." >&2
  exit 1
fi
if ! python3 -I check.py; then
  echo "--- flex-1 logs (tail) ---"; docker compose logs --tail 40 flex-1
  exit 1
fi
# The same gateway, driven by the official MCP Python and TypeScript SDKs. SKIP_SDK=1 skips them
# (they install from PyPI and npm).
if [ "${SKIP_SDK:-}" = "" ] && ! { docker compose run --rm sdk && docker compose run --rm ts-sdk; }; then
  echo "--- flex-1 logs (tail) ---"; docker compose logs --tail 40 flex-1
  exit 1
fi
