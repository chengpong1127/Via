#!/usr/bin/env bash
set -euo pipefail

installer="$(mktemp)"
trap 'rm -f "$installer"' EXIT
curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh -o "$installer"
sh "$installer"

export PATH="$HOME/.local/bin:$PATH"
codegraph install --yes
codegraph init --yes
