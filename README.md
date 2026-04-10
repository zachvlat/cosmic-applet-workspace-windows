# COSMIC Applet: Workspace Windows

This applet adds a per-output window list to the COSMIC panel. Each instance shows the windows in
the currently active workspace for that panel's output, and highlights the active window.

## Screenshot

Example applet layout in the COSMIC panel:

![Workspace Windows applet screenshot](docs/screenshots/workspace-windows-example.png)

## Build

```bash
cargo build --release
```

## Install locally

```bash
./scripts/install-local.sh
```

Or with `just`:

```bash
just install
```

Then restart the panel:

```bash
./scripts/restart-panel.sh
```

Or:

```bash
just restart-panel
```

The install script writes an absolute `Exec=` path into the local desktop file because some COSMIC
sessions do not include `~/.local/bin` in the panel process `PATH`.

After that, add `Workspace Windows` from COSMIC's panel applet settings.
