#!/usr/bin/env bash
# Dependency-direction guard: enforce Mera's layering machine-side.
#
# Checks the *normal* dependency graph with all features enabled, on every
# target platform, so an optional or platform-only edge counts too.
# Dev-dependencies are exempt by design: tests may wire layers together,
# shipped code may not.
#
# Active workspace crates (8):
#   leaves     mera-core, mod-sdk, mera-package-installer
#   domain     mera-config (reaches only mera-core)
#   host       mera-app-host
#   adapter    mera-agent-adapter (may depend on mera-app-host)
#   entries    mera (src-tauri)
#   mods       mods/text (text-mod; direct normal deps only mod-sdk or general libraries)
#
# Sealed AIGC/Worker/static-workflow crates under `crates/aigc/` and GPU/Burn
# crates (`burn-*`, `cubecl*`, `wgpu`) are strictly forbidden from the normal
# dependency graph of every active workspace member.
#
# No workspace crate depends on an agent stack (mera-agent-*, the legacy
# agent kernel crates, causa*). The operation pipeline in app-host serves
# humans and agents alike; an agent integration is an operator adapter above
# it, and the change that adds one grants that crate the only exception:
# `mera-agent-adapter` may depend on app-host, and only the desktop entry
# point `mera` may depend on it. Its name still contains the agent-stack
# prefix, so the agent check below skips that one edge; the adapter's own
# graph is checked against the same agent-stack pattern (minus itself), and
# every other member is explicitly banned from depending on it.
#
# Patterns are extended regexes matched against cargo-tree lines after the
# "-- " edge marker, so the crate itself (the root line) never matches.
set -euo pipefail

WORKSPACE='(mera(-[a-z-]+)?|aigc|mod-sdk) v'
TAURI='tauri(-[a-z-]+)? v'
AGENT='(mera-agent-[a-z-]+|mera-ai-protocol|mera-context-kernel|causa(-[a-z]+)?) v'
ADAPTER='mera-agent-adapter v'
APP_HOST='mera-app-host v'
ENTRIES='mera v'
SEALED='(aigc|mera-runtime|mera-nodes|mera-inference|mera-inference-burn|mera-inference-burn-worker|mera-model-manager|mera-model-acquisition|mera-backend-worker-protocol|mera-backend-worker-host|burn(-[a-z]+)?|cubecl(-[a-z]+)?|wgpu) v'
HTTP_SERVER='axum v'

tree_of() {
  cargo tree -p "$1" --edges normal --all-features --target all --locked --charset ascii
}

# check CRATE PATTERN... — CRATE's graph reaches none of PATTERN.
check() {
  local root="$1"
  shift
  local tree
  tree=$(tree_of "$root")
  local banned found
  for banned in "$@"; do
    found=$(grep -oE -- "-- ${banned}" <<<"$tree" | head -n 1 || true)
    if [[ -n "$found" ]]; then
      echo "FAIL: $root must not depend on ${found#-- }" >&2
      exit 1
    fi
  done
  echo "ok: $root"
}

# check_agent CRATE PATTERN... — like check, but the operator adapter edge is
# skipped. `mera-agent-adapter` is the one agent-stack name an operator
# adapter is allowed to depend on, and only `mera` may reach it; the
# adapter's own graph still goes through this check, and the loop below bans
# the adapter for every other member. The pattern can then keep matching the
# whole `mera-agent-*` family.
check_agent() {
  local root="$1"
  shift
  local tree
  tree=$(tree_of "$root" | grep -vE -- "-- ${ADAPTER}" || true)
  local banned found
  for banned in "$@"; do
    found=$(grep -oE -- "-- ${banned}" <<<"$tree" | head -n 1 || true)
    if [[ -n "$found" ]]; then
      echo "FAIL: $root must not depend on ${found#-- }" >&2
      exit 1
    fi
  done
  echo "ok: $root"
}

members=$(cargo metadata --no-deps --format-version 1 --locked |
  python3 -c 'import json, sys; print("\n".join(sorted(p["name"] for p in json.load(sys.stdin)["packages"])))')
if [[ -z "$members" ]]; then
  echo "FAIL: cargo metadata listed no workspace members" >&2
  exit 1
fi

# Mods under mods/ are first-party extensions. A normal direct dependency may
# be the mod contract (`mod-sdk`) or a general library, but never another
# kernel crate, aigc, the desktop shell or a backend. Dev and build
# dependencies are exempt: tests may compose the host, and a mod ships as its
# own binary. `cargo metadata --no-deps` reads direct edges, so no transitive
# crate can hide one.
cargo metadata --no-deps --format-version 1 --locked |
  python3 -c '
import json, os, re, sys

metadata = json.load(sys.stdin)
root = os.path.realpath(metadata["workspace_root"])
banned = re.compile(r"^(mera($|-)|aigc$|tauri|burn($|-)|cubecl($|-)|wgpu$)")
violations = []
members = 0
for package in metadata["packages"]:
    crate_dir = os.path.dirname(os.path.realpath(package["manifest_path"]))
    relative = os.path.relpath(crate_dir, root)
    if relative != "mods" and not relative.startswith("mods" + os.sep):
        continue
    members += 1
    for dependency in package["dependencies"]:
        if dependency["kind"] is not None:
            continue
        name = dependency["name"]
        if name == "mod-sdk" or not banned.match(name):
            continue
        violations.append("{} -> {}".format(package["name"], name))

if violations:
    for violation in violations:
        print("FAIL: mods/ direct dependency not allowed: " + violation, file=sys.stderr)
    sys.exit(1)
print("ok: mods/ ({} member(s), direct normal dependencies)".format(members))
'

# Leaves: domain vocabulary, mod contract, and package installer.
check mera-core "$WORKSPACE" 'reqwest v' "$HTTP_SERVER" "$TAURI" "$SEALED"
check mod-sdk 'mera-config v' 'mera-package-installer v' "$APP_HOST" "$ADAPTER" "$ENTRIES" "$TAURI" "$HTTP_SERVER" "$SEALED"
check mera-package-installer "$WORKSPACE" 'reqwest v' "$HTTP_SERVER" "$TAURI" "$SEALED"

# Config: reaches only mera-core among workspace crates.
check mera-config 'mod-sdk v' 'mera-package-installer v' "$APP_HOST" "$ADAPTER" "$ENTRIES" "$TAURI" "$HTTP_SERVER" "$SEALED"

# Every active crate stays free of the sealed AIGC/worker/Burn stack and
# respects agent-stack, Tauri, app-host, and entry boundaries.
while read -r crate; do
  case "$crate" in
    mera)
      check_agent "$crate" "$AGENT" "$ENTRIES" "$SEALED"
      ;;
    mera-agent-adapter)
      # The operator adapter may depend on app-host, but on nothing agent
      # stack, Tauri, an entry point, or sealed crates.
      check_agent "$crate" "$AGENT" "$TAURI" "$ENTRIES" "$SEALED"
      ;;
    *)
      check_agent "$crate" "$AGENT" "$TAURI" "$APP_HOST" "$ENTRIES" "$SEALED"
      check "$crate" "$ADAPTER"
      ;;
  esac
done <<<"$members"

echo "dependency directions OK"
