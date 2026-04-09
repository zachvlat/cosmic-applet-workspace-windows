#!/usr/bin/env sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

app_id="io.github.tkilian.CosmicAppletAppTitle"
binary_src="$project_dir/target/release/cosmic-applet-app-title"
binary_dst="$HOME/.local/bin/cosmic-applet-app-title"
desktop_src="$project_dir/data/$app_id.desktop"
desktop_dst="$HOME/.local/share/applications/$app_id.desktop"
icon_src="$project_dir/data/icons/scalable/apps/$app_id.svg"
icon_dst="$HOME/.local/share/icons/hicolor/scalable/apps/$app_id.svg"

if [ ! -x "$binary_src" ]; then
    printf '%s\n' "missing release binary: $binary_src" >&2
    printf '%s\n' "run: cargo build --release" >&2
    exit 1
fi

install -Dm755 "$binary_src" "$binary_dst"
install -Dm644 "$icon_src" "$icon_dst"
install -Dm644 "$desktop_src" "$desktop_dst"

tmp_desktop=$(mktemp)
trap 'rm -f "$tmp_desktop"' EXIT INT TERM
sed "s|^Exec=.*|Exec=$binary_dst|" "$desktop_src" >"$tmp_desktop"
install -Dm644 "$tmp_desktop" "$desktop_dst"

printf '%s\n' "installed $app_id"
printf '%s\n' "desktop entry: $desktop_dst"
printf '%s\n' "binary: $binary_dst"
