#!/usr/bin/env bash
set -euo pipefail

tool_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_dir="$(cd -- "$tool_dir/../.." && pwd)"
export LC_ALL=C.UTF-8

cd "$repository_dir"
cargo run --locked -p ephervex-screenshots --features skia -- generate "$@"
