<div align="center">

<img src=".github/icon.png" width="160" height="160" alt="Cleaner" />

# Cleaner

**The uninstaller macOS deserves, in your terminal.**

Complete uninstall · Restorable cleanup · Free forever

</div>

---

## Install

```bash
cargo install --git https://github.com/rokartur/cleaner
cleaner
```

> Requires macOS and Rust 1.88 or later.

## Features

- **Complete app uninstall.** Pick an app and Cleaner finds the app plus everything it left behind: support files, caches, preferences, saved state, logs, containers, launch agents and CLI links. Files matched by bundle id or full name are selected; vendor folders, names that only start with the app's name and hits inside another app's folder stay unselected for review. Files are grouped by location, folded until you open a group.
- **System Junk.** User caches, logs, saved state and developer caches. Regenerable caches are selected; toolchains and package stores wait for you to tick them.
- **Orphaned Files.** Files of apps you already deleted, graded by confidence. Nothing is preselected; select all (`a`) takes only launch items whose program is gone.
- **Restorable cleanup.** Everything goes to the Trash, grouped in one `<app> — <date>` folder. Delete History lists every removal and puts files back where they were.
- **System files on request.** Press `s` to also scan `/Library`, `/etc` and `/var/log`. Root-owned files move with `sudo` after you confirm and see each path.

## Keys

| Key | Action |
|---|---|
| `1`-`4`, `tab` | switch page |
| `/` | filter apps |
| `S` | sort apps by name, size or install date |
| `enter` | scan an app's files; in a file list, open or fold a group |
| `space`, `a` | select one (a whole group on its header), select all safe items |
| `d` | move the selection to the Trash |
| `o` | reveal in Finder |
| `u` | restore a history entry |
| `s` | include system locations |
| `R` | rescan |
| `q` | quit |

## Privacy

No telemetry. No analytics. No account. Cleaner runs entirely on your Mac.

## Build from source

```bash
git clone https://github.com/rokartur/cleaner.git
cd cleaner
cargo test
cargo run --release
```

## Contributing

Issues and pull requests are welcome. For larger changes, open an issue first to discuss what you'd like to change.

## License

© 2026 Artur Rok. All rights reserved.
