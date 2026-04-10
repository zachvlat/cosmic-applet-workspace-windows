#!/usr/bin/env sh
set -eu

panel_bin=${COSMIC_PANEL_BIN:-$(command -v cosmic-panel || true)}

if [ -z "$panel_bin" ]; then
    printf '%s\n' "cosmic-panel not found in PATH" >&2
    exit 1
fi

pkill -x cosmic-panel 2>/dev/null || true

for _ in 1 2 3 4 5; do
    if ! pgrep -x cosmic-panel >/dev/null 2>&1; then
        break
    fi
    sleep 0.2
done

nohup "$panel_bin" >/tmp/cosmic-panel.log 2>&1 &

printf '%s\n' "restarted cosmic-panel"
