# Root filesystem of the dedicated "docklet" WSL2 distro:
# Debian 13 (trixie) slim + systemd + docker-ce from Docker's apt repository.
FROM debian:trixie-slim

ARG K3D_VERSION=v5.9.0

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl systemd systemd-sysv dbus iptables iproute2 kmod procps socat \
 && install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian trixie stable" > /etc/apt/sources.list.d/docker.list \
 && apt-get update && apt-get install -y --no-install-recommends \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin \
 && curl -fsSL -o /usr/local/bin/k3d "https://github.com/k3d-io/k3d/releases/download/${K3D_VERSION}/k3d-linux-amd64" \
 && chmod +x /usr/local/bin/k3d \
 && systemctl enable docker containerd \
 && systemctl mask systemd-resolved systemd-networkd NetworkManager \
    systemd-tmpfiles-setup systemd-tmpfiles-clean.timer systemd-tmpfiles-clean \
    systemd-tmpfiles-setup-dev-early systemd-tmpfiles-setup-dev tmp.mount \
 && mkdir -p /usr/local/share/docklet \
 && { dpkg-query -W -f='${Package} ${Version}\n' docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin socat systemd; \
      echo "k3d ${K3D_VERSION}"; } > /usr/local/share/docklet/versions.txt \
 && rm -rf /var/lib/apt/lists/*

COPY rootfs/ /
