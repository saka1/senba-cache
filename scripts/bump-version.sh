#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <patch|minor|major>"
  exit 1
}

[[ $# -eq 1 ]] || usage

BUMP_TYPE="$1"

REPO_ROOT="$(git rev-parse --show-toplevel)"
CARGO_TOML="${REPO_ROOT}/Cargo.toml"

if [[ -n $(git status --porcelain) ]]; then
  echo "Error: working tree is dirty. Commit or stash changes first." >&2
  exit 1
fi

# Workspace has multiple members; pick the senba package explicitly.
CURRENT=$(cargo metadata --format-version=1 --no-deps | jq -r '.packages[] | select(.name=="senba") | .version')
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

case "$BUMP_TYPE" in
  patch) PATCH=$((PATCH + 1)) ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  *) usage ;;
esac

NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"
echo "Bumping version: ${CURRENT} → ${NEW_VERSION}"

# Update root Cargo.toml. The senba [package] block contains the first
# `version = "..."` line in the file, so the `0,/.../` range is sufficient
# even though the file may contain other version fields below.
sed -i "0,/^version = \"${CURRENT}\"/s//version = \"${NEW_VERSION}\"/" "$CARGO_TOML"

# Update Cargo.lock
cargo check --quiet

# Commit and push
git add "$CARGO_TOML" "${REPO_ROOT}/Cargo.lock"
git commit -m "chore: bump version to ${NEW_VERSION}"
read -rp "Push to remote? [y/N] " ans
if [[ "$ans" =~ ^[Yy]$ ]]; then
  git push
  echo "Done: v${NEW_VERSION} (pushed)"
else
  echo "Done: v${NEW_VERSION} (not pushed)"
fi
