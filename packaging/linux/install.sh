#!/bin/sh
# Installs rudbman for the current user: the program tree, a desktop entry and
# icons. Run from the unpacked release directory. No root required.
set -eu

prefix="${XDG_DATA_HOME:-$HOME/.local/share}"
bindir="$HOME/.local/bin"
appdir="$prefix/rudbman"

here="$(cd "$(dirname "$0")" && pwd)"

# rudbman is not a single file: it looks for the bridge JAR at lib/ and the
# bundled Java runtime at runtime/, both relative to the executable, so the
# whole tree has to move together. An earlier install is removed rather than
# copied over, or files left behind by its runtime would still be found by the
# new one.
rm -rf "$appdir"
mkdir -p "$appdir"
cp -R "$here/rudbman" "$here/lib" "$here/runtime" "$appdir/"
chmod 755 "$appdir/rudbman"

# A link, not a copy: current_exe() resolves symlinks, so the binary still sees
# $appdir as its own directory and finds lib/ and runtime/ beside it.
mkdir -p "$bindir"
ln -sf "$appdir/rudbman" "$bindir/rudbman"

install -Dm644 "$here/com.aihouse.rudbman.desktop" "$prefix/applications/com.aihouse.rudbman.desktop"
install -Dm644 "$here/icons/rudbman-128.png" "$prefix/icons/hicolor/128x128/apps/rudbman.png"
install -Dm644 "$here/icons/rudbman-256.png" "$prefix/icons/hicolor/256x256/apps/rudbman.png"
install -Dm644 "$here/icons/rudbman.svg" "$prefix/icons/hicolor/scalable/apps/rudbman.svg"

# Refresh caches when the tools are around; harmless to skip.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/applications" || true
command -v gtk-update-icon-cache  >/dev/null 2>&1 && gtk-update-icon-cache -q "$prefix/icons/hicolor" || true

echo "installed rudbman to $appdir, linked from $bindir (make sure it is on your PATH)"
