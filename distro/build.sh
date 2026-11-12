#!/usr/bin/env bash
# Builds the docklet rootfs for one base: buildx -> tar -> gzip -> sha256.
# Usage: build.sh [BASE_NAME]   (names are listed in bases.txt; default: first)
# Output: out/rootfs-<base>.tar.gz, .sha256, versions-<base>.txt
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bases="$here/bases.txt"
name="${1:-$(grep -v '^#' "$bases" | awk 'NF {print $1; exit}')}"

read -r _ image repo suite < <(grep -v '^#' "$bases" | awk -v n="$name" '$1 == n')
if [[ -z "${image:-}" ]]; then
  echo "unknown base '$name'; known bases:" >&2
  grep -v '^#' "$bases" | awk 'NF {print "  " $1}' >&2
  exit 2
fi

out="$here/out"
mkdir -p "$out"
tarball="$out/rootfs-$name.tar"
rm -f "$tarball" "$tarball.gz" "$tarball.gz.sha256"

docker buildx build --platform linux/amd64 \
  --build-arg "BASE_IMAGE=$image" \
  --build-arg "BASE_NAME=$name" \
  --build-arg "DOCKER_REPO=$repo" \
  --build-arg "DOCKER_SUITE=$suite" \
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
