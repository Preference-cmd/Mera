#!/usr/bin/env bash
# Dependency-direction guard: enforce Mera's layering machine-side.
#
# Checks the *normal* dependency graph with all features enabled, on every
# target platform, so an optional or platform-only edge counts too.
# Dev-dependencies are exempt by design: tests may wire layers together,
# shipped code may not.
#
#   leaves     core, mod-sdk, backend-worker-protocol core/protocol no workspace crate; mod-sdk reaches only core
#   domain     config, nodes, model-manager, model-acquisition,
#              inference, runtime
#   modules    aigc (inference composition, workers, model services)
#   backends   inference-burn, inference-burn-worker
#   workers    backend-worker-host (the wire protocol is a leaf above)
#   host       app-host
#   entries    mera (src-tauri)
#
# In kernel-only builds (--no-default-features), app-host and mera
# must not reach the optional aigc module.
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
# tonic 0.14's gRPC server (the worker contract's server) depends on axum
# internally. The HTTP_SERVER ban still guards against direct HTTP-server
# usage: the worker crates' checks strip tonic's subtree from the tree, so
# an axum edge reachable any other way still fails.
#
# Patterns are extended regexes matched against cargo-tree lines after the
# "-- " edge marker, so the crate itself (the root line) never matches.
# End a crate name with " v" to stop it matching longer names:
# "mera-inference v" does not match "mera-inference-burn v".
set -euo pipefail

WORKSPACE='(mera(-[a-z-]+)?|aigc|mod-sdk) v'
TAURI='tauri(-[a-z-]+)? v'
AGENT='(mera-agent-[a-z-]+|mera-ai-protocol|mera-context-kernel|causa(-[a-z]+)?) v'
ADAPTER='mera-agent-adapter v'
APP_HOST='mera-app-host v'
ENTRIES='mera v'
BACKENDS='(mera-inference-(burn|burn-worker)|burn-[a-z]+|cubecl(-[a-z]+)?|wgpu) v'
HTTP_SERVER='axum v'

tree_of() {
  cargo tree -p "$1" --edges normal --all-features --target all --locked --charset ascii
}

# Strip a named package's subtree from ASCII `cargo tree` output.
without_subtree() { # ... | without_subtree PACKAGE  (tree text on stdin)
  awk -v root=" $1 v" '
    function depth(line) {
      n = 0
      while (substr(line, 1, 4) == "|   " || substr(line, 1, 4) == "    ") {
        n++
        line = substr(line, 5)
      }
      return n
    }
    BEGIN { skipping = -1 }
    {
      if (skipping >= 0) {
        if (depth($0) > skipping) next
        skipping = -1
      }
      if (index($0, root) > 0) {
        skipping = depth($0)
        next
      }
      print
    }
  '
}

# check_without_tonic CRATE PATTERN... — like check, but the tree is
# matched with tonic's subtree removed (see the HTTP_SERVER note above).
check_without_tonic() {
  local root="$1"
  shift
  local tree found
  tree=$(tree_of "$root" | without_subtree tonic)
  local banned
  for banned in "$@"; do
    found=$(grep -oE -- "-- ${banned}" <<<"$tree" | head -n 1 || true)
    if [[ -n "$found" ]]; then
      echo "FAIL: $root must not depend on ${found#-- } (outside tonic)" >&2
      exit 1
    fi
  done
  echo "ok: $root"
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

# Leaves: the domain vocabulary, the mod contract and the worker wire
# protocol. The domain root also stays free of HTTP and the desktop shell.
check mera-core "$WORKSPACE" 'reqwest v' "$HTTP_SERVER" "$TAURI"
check mod-sdk "$BACKENDS" "$HTTP_SERVER" 'mera-runtime v' 'mera-inference v' 'mera-model-manager v' 'mera-model-acquisition v' 'mera-nodes v' 'mera-config v' 'mera-backend-worker-host v' 'aigc v'
check mera-backend-worker-protocol "$WORKSPACE"

# Domain crates and contracts never reach a concrete inference backend,
# a host or the desktop shell. The runtime additionally
# stays out of model management, and the inference contract stays
# runtime-neutral.
for crate in mera-config mera-nodes mera-model-manager mera-model-acquisition; do
  check "$crate" "$BACKENDS" "$HTTP_SERVER"
done
check mera-inference "$BACKENDS" "$HTTP_SERVER" 'mera-runtime v'
check mera-runtime "$BACKENDS" "$HTTP_SERVER" 'mera-model-manager v'

# Backends implement the inference contract and know nothing above it.
# The worker binary reaches tonic's gRPC transport (the worker contract),
# which carries axum internally — see the HTTP_SERVER note above.
for crate in mera-inference-burn mera-inference-burn-worker; do
  check_without_tonic "$crate" "$HTTP_SERVER" 'mera-runtime v' 'mera-model-manager v'
done

# The worker host manages out-of-process workers and never links a backend.
# The wire-protocol crate (the merged transports) is a workspace leaf, so
# the leaf check above already keeps it away from domain crates and backends.
check_without_tonic mera-backend-worker-host "$BACKENDS" "$HTTP_SERVER"

# Every crate stays free of the agent stack, except that the desktop entry
# point may depend on the one operator adapter. Only the desktop entry point
# uses Tauri; nothing below the entry points depends on the host facade;
# nothing depends on an entry point; and no other member depends on the
# operator adapter.
while read -r crate; do
  case "$crate" in
    mera)
      check_agent "$crate" "$AGENT" "$ENTRIES"
      ;;
    mera-agent-adapter)
      # The operator adapter may depend on app-host, but on nothing agent
      # stack, Tauri or an entry point.
      check_agent "$crate" "$AGENT" "$TAURI" "$ENTRIES"
      ;;
    *)
      check_agent "$crate" "$AGENT" "$TAURI" "$APP_HOST" "$ENTRIES"
      check "$crate" "$ADAPTER"
      ;;
  esac
done <<<"$members"

# Kernel-only assertion: app-host and entry crates compile without the
# optional aigc module when built with --no-default-features.
for crate in mera-app-host mera; do
  tree=$(cargo tree -p "$crate" --no-default-features --edges normal --target all --locked --charset ascii)
  if grep -E -- "aigc v" <<<"$tree" >/dev/null; then
    echo "FAIL: $crate --no-default-features must not depend on aigc" >&2
    exit 1
  fi
  echo "ok: $crate (kernel-only)"
done

echo "dependency directions OK"
