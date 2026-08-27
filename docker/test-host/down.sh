#!/usr/bin/env bash
# Tear down the disposable git-ark test host.
set -euo pipefail
if docker rm -f git-ark-testhost >/dev/null 2>&1; then
  echo "test host removed"
else
  echo "no test host running"
fi
