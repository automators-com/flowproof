#!/usr/bin/env bash
# Run untrusted third-party corpus code in a disposable rootless container.
#
# Blocker #2 from the concept: a corpus repo's `npm install` executes arbitrary
# postinstall scripts. This box holds ~/.ssh/flowproof_deploy and a gh token, so
# a stranger's postinstall must never share a trust domain with them. That risk
# is not recoverable by revert, which makes it the most serious in the design.
#
# Two-phase network, matching the concept:
#   --phase install : egress allowed (dependency fetch, model recording)
#   --phase replay  : egress DENIED  (proves flowproof's zero-LLM-call claim)
#
# Usage:
#   sandbox-run.sh --phase install --work /path/to/repo -- npm ci
#   sandbox-run.sh --phase replay  --work /path/to/repo -- flowproof run flow.yaml
set -euo pipefail

IMAGE="${SANDBOX_IMAGE:-docker.io/library/node:22-bookworm-slim}"
PHASE="install"
WORK=""
MEM="${SANDBOX_MEM:-2g}"
CPUS="${SANDBOX_CPUS:-1.5}"
PIDS="${SANDBOX_PIDS:-512}"
TIMEOUT="${SANDBOX_TIMEOUT:-900}"

while [ $# -gt 0 ]; do
  case "$1" in
    --phase) PHASE="$2"; shift 2 ;;
    --work)  WORK="$2";  shift 2 ;;
    --image) IMAGE="$2"; shift 2 ;;
    --)      shift; break ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

[ -n "$WORK" ] || { echo "--work <dir> is required" >&2; exit 2; }
[ $# -gt 0 ]   || { echo "no command given after --" >&2; exit 2; }
[ -d "$WORK" ] || { echo "work dir does not exist: $WORK" >&2; exit 2; }

# The work dir must be a scratch path, never the repo or the home dir. A typo
# here would mount the credentials we are trying to isolate.
case "$(realpath "$WORK")" in
  /home/flow/worktrees/*|/home/flow/corpus/*|/tmp/*) ;;
  *) echo "refusing to mount $(realpath "$WORK"): work dirs must live under
     /home/flow/worktrees, /home/flow/corpus, or /tmp" >&2; exit 2 ;;
esac

case "$PHASE" in
  install) NET=(--network=slirp4netns) ;;
  replay)  NET=(--network=none) ;;
  *) echo "--phase must be 'install' or 'replay'" >&2; exit 2 ;;
esac

# No -e flags anywhere: the container inherits none of this shell's environment,
# so FLOWPROOF_LOOP_TOKEN / ANTHROPIC_API_KEY / SSH_AUTH_SOCK cannot leak in.
# Only $WORK is mounted, and nothing else from the host filesystem.
exec timeout --signal=KILL "$TIMEOUT" \
  podman run --rm \
    "${NET[@]}" \
    --volume "$WORK:/work:rw,Z" \
    --workdir /work \
    --memory "$MEM" \
    --cpus "$CPUS" \
    --pids-limit "$PIDS" \
    --cap-drop=ALL \
    --security-opt no-new-privileges \
    --read-only \
    --tmpfs /tmp:rw,size=512m,exec \
    --tmpfs /run:rw,size=64m \
    "$IMAGE" \
    "$@"
