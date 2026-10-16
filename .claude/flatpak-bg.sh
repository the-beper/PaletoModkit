#!/bin/sh
# Starts the Flatpak build in the background, at low priority and on all but a few
# cores, so the machine stays usable. The bundle lands in dist/ as with
# packaging/build-flatpak.sh.
#   FREE_CPUS=n  cores left free (default 4)
# Progress: target/flatpak-build.log; its last line says "exit: <code>" once done.
# Does nothing if a build started by this script is still running.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
mkdir -p target
LOG=target/flatpak-build.log
PIDFILE=target/flatpak-build.pid

if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "a Flatpak build is already running (pid $(cat "$PIDFILE")): $LOG"
    exit 0
fi

CPUS=$(($(nproc) - ${FREE_CPUS:-4}))
[ "$CPUS" -ge 1 ] || CPUS=1
export CPUS

nohup sh -c 'nice -n 19 ionice -c3 ./packaging/build-flatpak.sh; echo "exit: $?"' >"$LOG" 2>&1 &
echo $! >"$PIDFILE"
echo "Flatpak build started in the background on $CPUS cores (pid $!): $LOG"
