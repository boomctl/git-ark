#!/usr/bin/env bash
set -euo pipefail
# Deploy git-ark to the host. Run from the repo root on the Mac.
#   HOST=youruser@your-host REPOS_ROOT=/home/youruser/git-ark/repos \
#     ./scripts/install-nas.sh
: "${HOST:?set HOST (e.g. youruser@your-host)}"
REMOTE_DIR="${REMOTE_DIR:-git-ark}"          # relative to the remote home
REPOS_ROOT="${REPOS_ROOT:?set REPOS_ROOT (absolute path on the host)}"
BIN=target/x86_64-unknown-linux-musl/release/git-ark

test -f "$BIN" || { echo "build first (see docs/deploy.md)"; exit 1; }

echo ">> creating remote layout"
ssh "$HOST" "mkdir -p '$REMOTE_DIR/bin' '$REPOS_ROOT'"

echo ">> copying binary + example config (edit config.toml on the host)"
scp "$BIN" "$HOST:$REMOTE_DIR/bin/git-ark"
scp config.example.toml "$HOST:$REMOTE_DIR/config.example.toml"
ssh "$HOST" "chmod 755 '$REMOTE_DIR/bin/git-ark'"

# sshd does NOT expand $HOME inside an authorized_keys forced command, so the
# instructions below must print the fully-resolved absolute remote path.
REMOTE_HOME=$(ssh "$HOST" 'echo $HOME')

echo
echo ">> NEXT (manual, one-time):"
echo "   1. On the host: cp $REMOTE_HOME/$REMOTE_DIR/config.example.toml $REMOTE_HOME/$REMOTE_DIR/config.toml and edit it"
echo "      (set repos_root=$REPOS_ROOT, age_recipient=<your age pubkey>, [s3] bucket/region/prefix)."
echo "   2. Create $REMOTE_HOME/$REMOTE_DIR/secrets.toml (chmod 600) with the write-only AWS keys."
echo "   3. Generate a dedicated key on the Mac:  ssh-keygen -t ed25519 -f ~/.ssh/git-ark -C git-ark"
echo "   4. Append to the host ~/.ssh/authorized_keys (single line):"
echo "      command=\"$REMOTE_HOME/$REMOTE_DIR/bin/git-ark shell --config $REMOTE_HOME/$REMOTE_DIR/config.toml\",no-pty,no-port-forwarding,no-agent-forwarding,no-X11-forwarding <paste ~/.ssh/git-ark.pub>"
echo "   5. On the Mac ~/.ssh/config add:"
echo "        Host git-ark"
echo "          HostName your-host"
echo "          User youruser"
echo "          IdentityFile ~/.ssh/git-ark"
