#!/usr/bin/env bash
# Launches the README headline benchmark on AWS via SkyPilot, fetches
# results.csv + cpu-topology.txt into ./data/, and tears the cluster down.
#
# Environment overrides:
#   REGION    AWS region (default: us-west-2)
#   USE_SPOT  set to "0" to use on-demand instead of spot (default: spot)
#   CLUSTER   SkyPilot cluster name (default: senba-readme-bench)

set -euo pipefail
cd "$(dirname "$0")"

REGION="${REGION:-us-west-2}"
USE_SPOT="${USE_SPOT:-1}"
CLUSTER="${CLUSTER:-senba-readme-bench}"

mkdir -p data

REPO_ROOT="$(git rev-parse --show-toplevel)"
SKY=(uv run --project "$REPO_ROOT/scripts" sky)

# Guarantee teardown on every exit path. `|| true` keeps the trap from
# masking the original failure if down itself errors.
cleanup() {
  "${SKY[@]}" down --yes "$CLUSTER" || true
}
trap cleanup EXIT

spot_args=()
if [ "$USE_SPOT" = "1" ]; then
  spot_args=(--use-spot)
fi

"${SKY[@]}" launch -c "$CLUSTER" bench.yml \
  --infra "aws/$REGION" \
  "${spot_args[@]}" \
  --idle-minutes-to-autostop 30 \
  --yes

# SkyPilot registers the cluster name as an SSH config alias, so plain rsync
# works (there is no `sky rsync` subcommand).
rsync -Pavz "$CLUSTER":~/results.csv data/results.csv
rsync -Pavz "$CLUSTER":~/cpu-topology.txt data/cpu-topology.txt

echo "wrote $(wc -l < data/results.csv) lines to data/results.csv"
