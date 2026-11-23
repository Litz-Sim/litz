#!/usr/bin/env bash
# Publish every workspace crate whose version is not on crates.io yet.
#
# A tag can point at a commit whose earlier run already reached some crates, so
# each one is checked against the registry first. The order comes from the
# workspace graph, dev-dependencies included, and skips publish = false members.
# `--no-verify` omits the per-crate verification build: CI ran the suite on the
# commit the tag names.
#
# CARGO_REGISTRY_TOKEN comes from the publish workflow's trusted-publishing
# exchange, or from a developer's own token when run locally.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"

version=$(python3 - <<'PY'
import pathlib, re
text = pathlib.Path("Cargo.toml").read_text()
print(re.search(r'\[workspace\.package\].*?^version = "([^"]+)"', text, re.S | re.M).group(1))
PY
)

order=$(python3 - <<'PY'
import json, subprocess

meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--no-deps", "--format-version", "1"]
))
packages = {
    package["name"]: package
    for package in meta["packages"]
    if package.get("publish") != []
}

seen, order = set(), []


def visit(name):
    if name in seen:
        return
    seen.add(name)
    for dependency in packages[name]["dependencies"]:
        # Dev-dependencies count here: `cargo publish` resolves them against the
        # registry too, so a crate whose dev-dependency is unpublished is
        # refused at packaging, even though it would not gate the build.
        if dependency["name"] in packages:
            visit(dependency["name"])
    order.append(name)


for name in sorted(packages):
    visit(name)
print("\n".join(order))
PY
)

# `--list` is the checklist for the one-time trusted-publisher setup: every
# name here needs this workflow as its publisher on crates.io.
if [ "${1:-}" = "--list" ]; then
  echo "$order"
  exit 0
fi

: "${CARGO_REGISTRY_TOKEN:?no crates.io token; the workflow supplies one through OIDC}"

for name in $order; do
  code=$(curl -s -o /dev/null -w '%{http_code}' \
    -H 'User-Agent: litz-release (https://github.com/Litz-Sim/litz)' \
    "https://crates.io/api/v1/crates/$name/$version")
  if [ "$code" = 200 ]; then
    echo "already published: $name $version"
    continue
  fi

  attempt=1
  until cargo publish -p "$name" --locked --no-verify; do
    if [ "$attempt" -ge 6 ]; then
      echo "could not publish $name" >&2
      exit 1
    fi
    echo "$name is not ready; retrying in 15s ($attempt/6)" >&2
    attempt=$((attempt + 1))
    sleep 15
  done

  # The next crate resolves this one through the registry, so wait for the
  # index rather than racing it.
  for _ in $(seq 1 40); do
    code=$(curl -s -o /dev/null -w '%{http_code}' \
      -H 'User-Agent: litz-release (https://github.com/Litz-Sim/litz)' \
      "https://crates.io/api/v1/crates/$name/$version")
    [ "$code" = 200 ] && break
    sleep 5
  done
  if [ "$code" != 200 ]; then
    echo "$name was uploaded but never appeared in the index" >&2
    exit 1
  fi
  echo "published: $name $version"
done
