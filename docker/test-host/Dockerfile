# A disposable, sshable Linux box that stands in for a git-ark host, so
# `git-ark host add` can be exercised end to end without touching a real machine.
# Deliberately minimal and git-ark-free (a fresh host), with a recent git
# (bookworm ships 2.39, well above the 2.28 floor) and `df` from coreutils.
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server git ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd \
 && useradd -m -s /bin/bash ark

# The test's public key is injected into ark's authorized_keys at run time
# (see up.sh), so the image itself carries no secrets.
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
