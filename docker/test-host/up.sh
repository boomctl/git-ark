#!/usr/bin/env bash
# Bring up a disposable git-ark test host in Docker. Destroys any previous
# container of the same name first, so re-running gives a clean box. Prints how
# to reach it. Env: PORT (default 2222), KEYDIR (default ~/.config/git-ark-test).
set -euo pipefail

NAME="${NAME:-git-ark-testhost}"
PORT="${PORT:-2222}"
KEYDIR="${KEYDIR:-$HOME/.config/git-ark-test}"
KEY="$KEYDIR/id_testhost"
HERE="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "$KEYDIR"
if [ ! -f "$KEY" ]; then
  ssh-keygen -t ed25519 -N '' -f "$KEY" -C git-ark-testhost >/dev/null
  echo "generated test key: $KEY"
fi

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker build -q -t git-ark-testhost "$HERE" >/dev/null
# Optionally join a user-defined network (so it can reach a MinIO sidecar by
# name) — set NETWORK to enable; the container is reachable there as "$NAME".
# (An if/else rather than an array to avoid the empty-array-under-`set -u`
# unbound-variable quirk in macOS's bash 3.2 when NETWORK is unset.)
if [ -n "${NETWORK:-}" ]; then
  docker run -d --name "$NAME" -p "$PORT:22" \
    --network "$NETWORK" --network-alias "$NAME" git-ark-testhost >/dev/null
else
  docker run -d --name "$NAME" -p "$PORT:22" git-ark-testhost >/dev/null
fi

# Authorize the test key for interactive login as `ark` (the control channel).
docker exec "$NAME" bash -c 'mkdir -p /home/ark/.ssh && chmod 700 /home/ark/.ssh'
docker cp "$KEY.pub" "$NAME:/home/ark/.ssh/authorized_keys"
docker exec "$NAME" bash -c 'chmod 600 /home/ark/.ssh/authorized_keys && chown -R ark:ark /home/ark/.ssh'

# Wait for sshd to accept the key.
ssh_opts=(-p "$PORT" -i "$KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2)
for _ in $(seq 1 20); do
  ssh "${ssh_opts[@]}" ark@localhost true 2>/dev/null && break
  sleep 0.5
done

echo "test host up: ark@localhost:$PORT"
echo "  key:  $KEY"
echo "  ssh:  ssh ${ssh_opts[*]} ark@localhost"
