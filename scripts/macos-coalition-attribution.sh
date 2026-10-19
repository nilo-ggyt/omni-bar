#!/bin/bash
# Collect explicitly selected process identity and bounded raw coalition evidence without opening a privilege prompt.
set -euo pipefail

# Resolve the adjacent maintained helper without evaluating caller arguments as shell code.
exec /usr/bin/python3 "$(/usr/bin/dirname "$0")/macos_coalition_attribution.py" "$@"
