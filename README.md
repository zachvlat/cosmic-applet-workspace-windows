# COSMIC Applet: Active Window Title

This applet adds a text item to the COSMIC panel and updates it with the title of the currently
active window on the same output as the panel.

## Build

```bash
cargo build --release
```

## Install locally

```bash
./scripts/install-local.sh
```

Then restart the panel:

```bash
pkill -x cosmic-panel
```

The install script writes an absolute `Exec=` path into the local desktop file because some COSMIC
sessions do not include `~/.local/bin` in the panel process `PATH`.

After that, add `Active Window Title` from COSMIC's panel applet settings.
