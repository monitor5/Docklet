#!/usr/bin/env bash
# Builds the docklet rootfs for one base: buildx -> tar -> gzip -> sha256.
# Usage: build.sh [BASE_NAME]   (names are listed in bases.txt; default: first)
# Output: out/rootfs-<base>.tar.gz, .sha256, versions-<base>.txt
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bases="$here/bases.txt"
name="${1:-$(awk '!/^#/ && NF {print $1; exit}' "$bases")}"

# A row is: name recipe image [KEY=VALUE ...]
row="$(awk -v n="$name" '!/^#/ && $1 == n' "$bases")"
if [[ -z "$row" ]]; then
  echo "unknown base '$name'; known bases:" >&2
  awk '!/^#/ && NF {print "  " $1}' "$bases" >&2
  exit 2
fi
read -r -a fields <<<"$row"
if (( ${#fields[@]} < 3 )); then
  echo "bases.txt: the row for '$name' needs at least a recipe and a base image" >&2
  exit 2
fi
recipe="${fields[1]}"
image="${fields[2]}"
build_args=(
  --build-arg "BASE_IMAGE=$image"
  --build-arg "BASE_NAME=$name"
)
for arg in "${fields[@]:3}"; do
  if [[ ! "$arg" =~ ^[A-Z][A-Z0-9_]*=.+$ ]]; then
    echo "bases.txt: '$arg' in the row for '$name' is not KEY=VALUE" >&2
    exit 2
  fi
  build_args+=(--build-arg "$arg")
done

out="$here/out"
mkdir -p "$out"
tarball="$out/rootfs-$name.tar"
rm -f "$tarball" "$tarball.gz" "$tarball.gz.sha256"

echo "base $name: recipe $recipe, image $image"
docker buildx build --platform linux/amd64 \
  --target "$recipe" \
  "${build_args[@]}" \
  --output "type=tar,dest=$tarball" "$here"

# Microsoft's custom distro guidance: WSL generates resolv.conf itself.
tar --delete -f "$tarball" etc/resolv.conf 2>/dev/null || true

# Record what the image actually contains.
tar -xOf "$tarball" usr/local/share/docklet/versions.txt >"$out/versions-$name.txt"

gzip --best "$tarball"
(cd "$out" && sha256sum "rootfs-$name.tar.gz" >"rootfs-$name.tar.gz.sha256")

cat "$out/versions-$name.txt"
cat "$tarball.gz.sha256"
ls -lh "$out"
