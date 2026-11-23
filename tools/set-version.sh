#!/usr/bin/env bash
# Set the version of every workspace crate and the npm package.
#
# The version lives in two places in the root Cargo.toml: [workspace.package]
# carries the exact release, and [workspace.dependencies] carries the range
# every member asks for. A patch release keeps that range, so only the exact
# version changes; a new minor or major bumps both.
set -euo pipefail

version=${1:?usage: tools/set-version.sh 0.4.1}
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "not a three-part version: $version" >&2; exit 1 ;;
esac

python3 - "$version" <<'PY'
import json, pathlib, re, sys

version = sys.argv[1]
major, minor, _patch = version.split(".")
compatible = f"0.{minor}" if major == "0" else major
root = pathlib.Path.cwd()

cargo = root / "Cargo.toml"
text = cargo.read_text()


def section(name):
    match = re.search(rf"\[{re.escape(name)}\](.*?)(?=\n\[|\Z)", text, re.S)
    if match is None:
        sys.exit(f"{name} is missing from Cargo.toml")
    return match


match = section("workspace.package")
body = re.sub(r'^version = "[^"]+"', f'version = "{version}"', match.group(1), count=1, flags=re.M)
text = text[: match.start(1)] + body + text[match.end(1):]

match = section("workspace.dependencies")
body = re.sub(r'^(litz[a-z0-9-]* = \{ version = )"[^"]+"', rf'\1"{compatible}"', match.group(1), flags=re.M)
text = text[: match.start(1)] + body + text[match.end(1):]
cargo.write_text(text)

package = root / "js/package.json"
data = json.loads(package.read_text())
data["version"] = version
package.write_text(json.dumps(data, indent=2) + "\n")

print(f"crates and npm at {version}; internal requirements accept {compatible}.x")
PY
