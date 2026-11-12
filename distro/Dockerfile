# Root filesystem of the dedicated "docklet" WSL2 distro: a Debian-family base
# with systemd and docker-ce from Docker's apt repository. The base is a build
# argument (see bases.txt); nothing outside this file depends on which one is
# used. What Docklet requires from a rootfs is only:
#   - systemd starts dockerd on /var/run/docker.sock
#   - /usr/local/bin/docklet-relay exists
ARG BASE_IMAGE=debian:trixie-slim

# The stdio <-> docker.sock relay, linked statically so it runs on any base.
FROM rust:1-slim-trixie AS relay
RUN rustup target add x86_64-unknown-linux-musl
WORKDIR /src
COPY relay/ .
RUN cargo build --release --locked --target x86_64-unknown-linux-musl

FROM ${BASE_IMAGE}

ARG BASE_NAME=debian-13
ARG DOCKER_REPO=debian
ARG DOCKER_SUITE=trixie
ARG K3D_VERSION=v5.9.0
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl systemd systemd-sysv dbus iptables iproute2 kmod procps \
 && install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL "https://download.docker.com/linux/${DOCKER_REPO}/gpg" -o /etc/apt/keyrings/docker.asc \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/${DOCKER_REPO} ${DOCKER_SUITE} stable" > /etc/apt/sources.list.d/docker.list \
 && apt-get update && apt-get install -y --no-install-recommends \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin \
 && curl -fsSL -o /usr/local/bin/k3d "https://github.com/k3d-io/k3d/releases/download/${K3D_VERSION}/k3d-linux-amd64" \
 && chmod +x /usr/local/bin/k3d \
 && systemctl enable docker containerd \
 && systemctl mask systemd-resolved systemd-networkd NetworkManager \
    systemd-tmpfiles-setup systemd-tmpfiles-clean.timer systemd-tmpfiles-clean \
    systemd-tmpfiles-setup-dev-early systemd-tmpfiles-setup-dev tmp.mount \
 && mkdir -p /usr/local/share/docklet \
 && { echo "base ${BASE_NAME}"; \
      dpkg-query -W -f='${Package} ${Version}\n' docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin systemd; \
      echo "k3d ${K3D_VERSION}"; } > /usr/local/share/docklet/versions.txt \
 && rm -rf /var/lib/apt/lists/*

COPY --from=relay /src/target/x86_64-unknown-linux-musl/release/docklet-relay /usr/local/bin/docklet-relay
COPY rootfs/ /
