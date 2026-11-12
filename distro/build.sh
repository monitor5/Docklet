#!/usr/bin/env bash
# Builds the docklet rootfs: buildx -> tar -> gzip -> sha256.
# Output: distro/out/rootfs.tar.gz, rootfs.tar.gz.sha256, versions.txt
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/out"
rm -rf "$out"
mkdir -p "$out"

docker buildx build --platform linux/amd64 --output "type=tar,dest=$out/rootfs.tar" "$here"

# Microsoft's custom distro guidance: WSL generates resolv.conf itself.
tar --delete -f "$out/rootfs.tar" etc/resolv.conf 2>/dev/null || true

# Record what the image actually contains.
tar -xOf "$out/rootfs.tar" usr/local/share/docklet/versions.txt >"$out/versions.txt"

gzip --best "$out/rootfs.tar"
(cd "$out" && sha256sum rootfs.tar.gz >rootfs.tar.gz.sha256)

cat "$out/versions.txt"
cat "$out/rootfs.tar.gz.sha256"
ls -lh "$out"
