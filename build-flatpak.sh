#!/bin/bash
set -euo pipefail

APP_ID="io.github.tkilian.CosmicAppletWorkspaceWindows"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="${SCRIPT_DIR}/${APP_ID}.json"
BUILD_DIR="${SCRIPT_DIR}/builddir"
REPO_DIR="${SCRIPT_DIR}/repo"
OUTPUT_FILE="${SCRIPT_DIR}/${APP_ID}.flatpak"

cleanup() {
    rm -rf "$BUILD_DIR" "$REPO_DIR"/*.flatpak 2>/dev/null || true
}

build() {
    echo "=== Building release binary ==="
    cargo build --release

    echo "=== Setting up build directory ==="
    rm -rf "$BUILD_DIR"
    mkdir -p "$BUILD_DIR/files/bin"
    mkdir -p "$BUILD_DIR/files/share/applications"
    mkdir -p "$BUILD_DIR/files/share/icons/hicolor/scalable/apps"

    cp target/release/cosmic-applet-workspace-windows "$BUILD_DIR/files/bin/"
    cp data/*.desktop "$BUILD_DIR/files/share/applications/"
    cp data/icons/scalable/apps/*.svg "$BUILD_DIR/files/share/icons/hicolor/scalable/apps/"

    cat > "$BUILD_DIR/metadata" << EOF
[Application]
name=${APP_ID}
runtime=org.freedesktop.Platform/x86_64/24.08
runtime-version=24.08
sdk=org.freedesktop.Sdk/x86_64/24.08
command=cosmic-applet-workspace-windows
EOF

    echo "=== Finishing build ==="
    flatpak build-finish "$BUILD_DIR" \
        --share=ipc \
        --socket=wayland \
        --socket=pulseaudio \
        --device=dri \
        --share=network \
        --filesystem=xdg-config:ro \
        --filesystem=xdg-data:ro \
        --talk-name=org.freedesktop.Notifications \
        --own-name="${APP_ID}.*"

    echo "=== Exporting repository ==="
    mkdir -p "$REPO_DIR"
    flatpak build-export "$REPO_DIR" "$BUILD_DIR"

    echo "=== Creating bundle ==="
    rm -f "$OUTPUT_FILE"
    flatpak build-bundle "$REPO_DIR" "$OUTPUT_FILE" "$APP_ID"

    echo "=== Done! ==="
    ls -lh "$OUTPUT_FILE"
}

install_deps() {
    echo "=== Installing Flatpak dependencies ==="
    flatpak install -y --user flathub \
        org.freedesktop.Platform/x86_64/24.08 \
        org.freedesktop.Sdk/x86_64/24.08 \
        org.freedesktop.Sdk.Extension.rust-stable/x86_64/24.08
}

case "${1:-build}" in
    clean)
        cleanup
        echo "Cleaned build directories"
        ;;
    deps)
        install_deps
        ;;
    build)
        build
        ;;
    all)
        cleanup
        install_deps
        build
        ;;
    *)
        echo "Usage: $0 {clean|deps|build|all}"
        echo "  clean  - Clean build directories"
        echo "  deps   - Install Flatpak dependencies"
        echo "  build  - Build the Flatpak package"
        echo "  all    - Clean, install deps, and build"
        exit 1
        ;;
esac