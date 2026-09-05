# =============================================================================
# LinuxCNC simulator for the EMCO PC Turn 50, reachable from a browser.
#
# Runs the *uspace* build, which needs no realtime kernel and so works fine in
# a container. This simulates the lathe for training; it does NOT and cannot
# control the real machine — see docs/20-control-architecture.md.
# =============================================================================
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive \
    DISPLAY=:1 \
    LC_ALL=C.UTF-8

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg \
        xvfb x11vnc novnc websockify openbox \
        xterm x11-utils \
        python3 python3-tk \
        fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# LinuxCNC's own apt repository.
#
# The archive signing key is committed alongside this Dockerfile rather than
# fetched at build time: linuxcnc.org does not publish it at a stable URL, and
# pinning it here makes the build reproducible and the trust decision reviewable.
# It is the "EMC Archive Signing Key <emc-board@linuxcnc.org>",
# fingerprint EEDD0D29 F81DCAA0 D258661F 3CB9FD14 8F374FEF, obtained from
# keyserver.ubuntu.com. Verify it yourself before trusting this image:
#     gpg --show-keys sim/linuxcnc-archive-keyring.asc
#
# It is a DSA-1024 key from 2008, which Debian bookworm's apt considers weak,
# so the repository is accepted explicitly rather than silently. That is a
# deliberate, documented trade-off: it is the only key upstream signs with.
COPY linuxcnc-archive-keyring.gpg /usr/share/keyrings/linuxcnc-archive-keyring.gpg
# Note the components: "base" carries supporting files, "2.9-uspace" carries
# the uspace build itself. Without the second one apt silently falls back to
# Debian's own linuxcnc 2.9.0~pre1, which has mismatched HAL modules and fails
# at runtime with "undefined symbol: hal_ready".
RUN echo "deb [signed-by=/usr/share/keyrings/linuxcnc-archive-keyring.gpg] \
http://linuxcnc.org bookworm base 2.9-uspace" > /etc/apt/sources.list.d/linuxcnc.list \
 && printf 'Package: *\nPin: origin linuxcnc.org\nPin-Priority: 1001\n' \
        > /etc/apt/preferences.d/linuxcnc \
 && printf 'APT::Key::Assert-Pubkey-Algo "";\n' \
        > /etc/apt/apt.conf.d/99linuxcnc-weak-key \
 && apt-get update \
 && apt-get install -y --no-install-recommends linuxcnc-uspace \
 && rm -rf /var/lib/apt/lists/*

# LinuxCNC refuses to run as root.
RUN useradd -ms /bin/bash cnc

# /usr/bin/rtapi_app ships setuid root so it can claim realtime scheduling.
# In a container that backfires: it creates the HAL SysV shared-memory segment
# as root with mode 0600, and then the unprivileged process that actually runs
# LinuxCNC cannot attach to it —
#     rtapi_shmem_new failed due to shmget(key=0x48484c34): Permission denied
# which cascades into a pile of misleading "undefined symbol: hal_ready" errors.
#
# The uspace build does not need realtime scheduling to simulate a machine, so
# drop the setuid bit and let everything run as one ordinary user. Do NOT copy
# this into a config that drives real hardware — there you want the realtime
# privileges, and a realtime kernel to go with them.
RUN chmod u-s /usr/bin/rtapi_app
WORKDIR /home/cnc

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

USER cnc
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
