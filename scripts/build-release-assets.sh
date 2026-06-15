#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

out_dir="target/dist-assets"
completions_dir="$out_dir/completions"
man_dir="$out_dir/man"
rm -rf "$out_dir"
mkdir -p "$completions_dir" "$man_dir"

cargo build --release

root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

CCPLAN_ROOT="$root" target/release/ccplan completions bash > "$completions_dir/ccplan.bash"
CCPLAN_ROOT="$root" target/release/ccplan completions zsh > "$completions_dir/_ccplan"
CCPLAN_ROOT="$root" target/release/ccplan completions fish > "$completions_dir/ccplan.fish"
CCPLAN_ROOT="$root" target/release/ccplan completions powershell > "$completions_dir/_ccplan.ps1"

manpage="$(find target/release/build -path '*/out/ccplan.1' -type f -print | sort | tail -n 1)"
if [[ -z "$manpage" ]]; then
  echo "missing generated ccplan.1" >&2
  exit 1
fi
cp "$manpage" "$man_dir/ccplan.1"
