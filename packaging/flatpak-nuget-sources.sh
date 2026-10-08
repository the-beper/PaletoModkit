#!/bin/sh

set -eu
cd "$(dirname "$0")/.."

commit=41c20aa10819cdb2a4f3ca171758a96d1955c018
sha256=3ac5b42c46fd7c6e8c71bb8a4be7943e8125d637ea7e2c3b1a25668e4ac79e98
script=target/flatpak-dotnet-generator.py

mkdir -p target
if ! echo "$sha256  $script" | sha256sum -c --status 2>/dev/null; then
    curl -sfL -o "$script" \
        "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$commit/dotnet/flatpak-dotnet-generator.py"
    echo "$sha256  $script" | sha256sum -c --quiet
fi
if [ ! -f third_party/codewalker/CodeWalker.Core/CodeWalker.Core.csproj ]; then
    git submodule update --init third_party/codewalker
fi

if [ "$(stat -c %u "${HOME:-/nonexistent}" 2>/dev/null || echo none)" != "$(id -u)" ]; then
    export HOME="$PWD/target/dotnet-home"
    mkdir -p "$HOME"
fi

work=$PWD/target/nuget-restore
rm -rf "$work"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tools" "$work/third_party"
cp -r tools/cwconv "$work/tools/"
cp -r third_party/codewalker "$work/third_party/"
python3 "$script" packaging/nuget-sources.json "$work/tools/cwconv/paleto-cwconv.csproj" \
    --runtime linux-x64 --freedesktop 26.08 --dotnet 10 --dotnet-args -p:SelfContained=true

if [ "$(tr -d '[:space:]' < packaging/nuget-sources.json)" = "[]" ]; then
    echo "no NuGet package found: the restore failed" >&2
    exit 1
fi
echo "wrote packaging/nuget-sources.json"
