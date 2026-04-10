set shell := ["sh", "-cu"]

default:
    @just --list

build:
    cargo build --release

install: build
    ./scripts/install-local.sh

restart-panel:
    ./scripts/restart-panel.sh

install-restart: install
    ./scripts/restart-panel.sh
