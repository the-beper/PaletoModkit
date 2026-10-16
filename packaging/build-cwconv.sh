#!/bin/sh
# Builds paleto-cwconv (the Legacy -> Enhanced converter, CodeWalker based)
# into target/cwconv/, self-contained: the .NET runtime is bundled, so users
# do not need .NET installed.
#   OUT=dir  build somewhere else
# Uses the .NET 10 SDK when installed, otherwise its container (podman).
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-$ROOT/target/cwconv}
cd "$ROOT"
# podman reads a relative -v source as a volume name.
mkdir -p "$OUT" && OUT=$(cd "$OUT" && pwd)
if [ ! -f third_party/codewalker/CodeWalker.Core/CodeWalker.Core.csproj ]; then
    git submodule update --init third_party/codewalker
fi
ARGS="-c Release -r linux-x64 --self-contained true -p:DebugType=none"
if command -v dotnet >/dev/null 2>&1; then
    dotnet publish tools/cwconv/paleto-cwconv.csproj $ARGS -o "$OUT"
else
    mkdir -p "$OUT"
    # The build writes obj/ and bin/ next to the sources: build a copy.
    podman run --rm --security-opt label=disable -v "$ROOT:/src:ro" -v "$OUT:/out" \
        mcr.microsoft.com/dotnet/sdk:10.0 sh -c "
            set -e
            mkdir -p /b/tools /b/third_party
            cp -r /src/tools/cwconv /b/tools/
            cp -r /src/third_party/codewalker /b/third_party/
            dotnet publish /b/tools/cwconv/paleto-cwconv.csproj $ARGS -o /out"
fi
echo "built $OUT/paleto-cwconv"
