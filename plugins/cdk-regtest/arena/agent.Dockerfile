# cdk-regtest attacker image (absorbed from vul-lab, 2026-08-10).
#
# Toolchain policy: bash + Rust-compiled binaries only. There is
# intentionally NO Python (or any other interpreter) in this image.
# Compiled attack tools (cdk-cli, custom Rust harnesses) are mounted
# read-only at /opt/tools by `arena.sh agent`.
#
# Hardening is applied at runtime (see arena.sh): dropped capabilities,
# read-only root fs, no-new-privileges, resource caps. This Dockerfile only
# guarantees a non-root default user and a sane PID 1.
FROM debian:stable-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        jq \
        sqlite3 \
        socat \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash attacker

USER attacker
WORKDIR /work

# tini as PID 1: reaps zombies from fork-heavy attack scripts.
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/bin/bash"]