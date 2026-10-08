#!/bin/sh

set -eu
cd "$(dirname "$0")/.."

commit=41c20aa10819cdb2a4f3ca171758a96d1955c018
sha256=0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380
script=target/flatpak-cargo-generator.py

mkdir -p target
if ! echo "$sha256  $script" | sha256sum -c --status 2>/dev/null; then
    curl -sfL -o "$script" \
        "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$commit/cargo/flatpak-cargo-generator.py"
    echo "$sha256  $script" | sha256sum -c --quiet
fi
python3 "$script" Cargo.lock -o packaging/cargo-sources.json
