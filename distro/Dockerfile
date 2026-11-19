# syntax=docker/dockerfile:1
# Root filesystem of the dedicated "docklet" WSL2 distro.
#
# build.sh builds one recipe stage of this file per base (see bases.txt) and
# exports its filesystem as a tarball:
#   apt  Debian-family bases: systemd, docker-ce from Docker's apt repository
#   apk  Alpine: Docker from Alpine's packages, no init system; WSL's [boot]
#        command runs apk/docklet-boot, which starts dockerd
#
# The rootfs contract. Docklet's engine code never branches on the base; what
# it needs from a rootfs is only:
#   - dockerd running after the distro boots, on /var/run/docker.sock
#     (how it gets started is up to the recipe)
#   - /usr/local/bin/docklet-relay
#   - a `docker` CLI on PATH inside the distro: readiness is probed with
#     `docker info` and running containers are counted with `docker ps -q`
# Every recipe also installs k3d (same version everywhere), the shared files
# under rootfs/ and /usr/local/share/docklet/versions.txt.

# Set by build.sh from bases.txt; there is deliberately no default.
ARG BASE_IMAGE
ARG K3D_VERSION=v5.9.0

# The stdio <-> docker.sock relay, linked statically so it runs on any base.
FROM rust:1-slim-trixie AS relay
RUN rustup target add x86_64-unknown-linux-musl
WORKDIR /src
COPY relay/ .
RUN cargo build --release --locked --target x86_64-unknown-linux-musl

# k3d is a static Go binary, so one download serves every base.
FROM scratch AS k3d
ARG K3D_VERSION
ADD --chmod=0755 https://github.com/k3d-io/k3d/releases/download/${K3D_VERSION}/k3d-linux-amd64 /k3d

# Files every recipe installs unchanged. /etc/wsl.conf has no [boot] section
# here: each recipe appends the one that starts dockerd its way.
FROM scratch AS common
COPY --from=relay /src/target/x86_64-unknown-linux-musl/release/docklet-relay /usr/local/bin/docklet-relay
COPY --from=k3d /k3d /usr/local/bin/k3d
COPY rootfs/ /

# --- Recipe apt: Debian family, systemd starts dockerd -----------------------
FROM ${BASE_IMAGE} AS apt
ARG BASE_NAME
ARG DOCKER_REPO
ARG DOCKER_SUITE
ARG K3D_VERSION
ENV DEBIAN_FRONTEND=noninteractive
SHELL ["/bin/sh", "-eux", "-c"]

RUN : "${BASE_NAME:?}" "${DOCKER_REPO:?set in bases.txt}" "${DOCKER_SUITE:?set in bases.txt}"
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
    ca-certificates curl systemd systemd-sysv dbus iptables iproute2 kmod procps
RUN install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL "https://download.docker.com/linux/${DOCKER_REPO}/gpg" -o /etc/apt/keyrings/docker.asc \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/${DOCKER_REPO} ${DOCKER_SUITE} stable" > /etc/apt/sources.list.d/docker.list
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
RUN systemctl enable docker containerd \
 && systemctl mask systemd-resolved systemd-networkd NetworkManager \
    systemd-tmpfiles-setup systemd-tmpfiles-clean.timer systemd-tmpfiles-clean \
    systemd-tmpfiles-setup-dev-early systemd-tmpfiles-setup-dev tmp.mount
RUN mkdir -p /usr/local/share/docklet \
 && { echo "base ${BASE_NAME}"; \
      dpkg-query -W -f='${Package} ${Version}\n' docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin systemd; \
      echo "k3d ${K3D_VERSION}"; } > /usr/local/share/docklet/versions.txt \
 && cat /usr/local/share/docklet/versions.txt
RUN rm -rf /var/lib/apt/lists/*

COPY --from=common / /
RUN printf '\n[boot]\nsystemd=true\n' >> /etc/wsl.conf

# --- Recipe apk: Alpine, no init system --------------------------------------
FROM ${BASE_IMAGE} AS apk
ARG BASE_NAME
ARG K3D_VERSION
SHELL ["/bin/sh", "-eux", "-c"]

RUN : "${BASE_NAME:?}"
# Versions are not pinned; versions.txt records what was installed. The
# buildx and compose CLI plugins are left out: they would only serve a docker
# CLI run inside the distro, and the Windows-side CLI brings its own.
RUN apk add --no-cache \
    docker-engine docker-cli containerd runc iptables ca-certificates
# `apk list` prints "name-version arch {origin} (license) [installed]"; the
# version is the last two dash-separated fields (e.g. 29.9.3-r0). The check at
# the end fails the build if that format ever changes.
RUN mkdir -p /usr/local/share/docklet \
 && { echo "base ${BASE_NAME}"; \
      echo "alpine-release $(cat /etc/alpine-release)"; \
      apk list --installed docker-engine docker-cli containerd runc iptables busybox musl \
        | awk '/\[installed\]/ {print $1}' \
        | sed -E 's/-([^-]+-r[0-9]+)$/ \1/' \
        | sort; \
      echo "k3d ${K3D_VERSION}"; } > /usr/local/share/docklet/versions.txt \
 && cat /usr/local/share/docklet/versions.txt \
 && for package in docker-engine docker-cli containerd runc iptables busybox musl; do \
      grep -q "^${package} [0-9]" /usr/local/share/docklet/versions.txt \
        || { echo "versions.txt lacks ${package}" >&2; exit 1; }; \
    done

COPY --from=common / /
COPY --chmod=0755 apk/docklet-boot /usr/local/bin/docklet-boot
RUN printf '\n[boot]\ncommand=/usr/local/bin/docklet-boot\n' >> /etc/wsl.conf
