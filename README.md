# cleaner

A terminal UI that uninstalls Mac apps with all their leftovers, cleans junk and finds files of apps you already deleted.

```text
 Cleaner  1 Apps  115   2 Junk 30 GB   3 Orphans 2211   4 History 45
────────────────────────────────────────────────────────────────────────────────────────────────────
 Apps › BetterAudio 26.7-beta.4 · pro.  takes 1.7 GB · app 56 MB + 204 leftovers, 1.6 GB   / filter
  ▸ [x] Application          /Applications               1 of 1    56 MB  ▉                        █
▌ ▸ [x] Application Support  …ry/Application Support     3 of 3   963 KB
  ▾ [-] Caches               ~/Library/Caches            4 of 5   1.6 GB  ████████████████████████
    [x] pro.betteraudio.BetterAudio.debug                         1.4 GB  █████████████████████▍
    [x] pro.betteraudio.BetterAudio                                37 KB
────────────────────────────────────────────────────────────────────────────────────────────────────
 ~/Library/Application Support   3 items · 963 KB
 15 selected  1.5 GB                                                                   d  uninstall
────────────────────────────────────────────────────────────────────────────────────────────────────
 space select  enter fold  a safe only  / filter  o reveal  esc back  ? help  q quit
```

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/rokartur/cleaner/main/install.sh | sh
```

The script installs the binary for your Mac to `~/.local/bin`. To use another folder, set `CLEANER_INSTALL_DIR`. You can also download a binary from the [releases page](https://github.com/rokartur/cleaner/releases): arm64 and x86_64.

From source, with Rust 1.88 or newer:

```sh
cargo install --git https://github.com/rokartur/cleaner
```

## Usage

```sh
cleaner            # start the UI
cleaner update     # install the latest release now
cleaner --version
```

A release build checks GitHub at each start. When it finds a newer release, it installs it in the background, and the new version runs from the next start. A build from source never updates itself.

## Pages

- **Apps:** open an app to see what it takes on disk, the app plus every file it left behind. Matches by bundle id or full name are selected. Vendor folders and partial name matches stay unselected for you to check.
- **Junk:** user caches, logs, saved state and developer caches. Toolchains and package stores stay unselected.
- **Orphans:** files of apps you already deleted, graded by confidence. Nothing is preselected.
- **History:** every removal, with `u` to put the files back.

Every removal goes to the Trash, in one `<app> — <date>` folder. Press `s` to also scan `/Library`, `/etc` and `/var/log`. Root-owned files move with `sudo`, after you confirm and see each path.

## Keys

| Key | Action |
| --- | --- |
| `1`-`4`, `tab` | Switch page |
| `↑` `↓`, `j` `k`, `g` `G` | Move, jump to the top or bottom |
| `enter`, `esc` | Open an app or a group, go back |
| `space`, `a` | Select an item or a group, select only the safe items |
| `d` | Move the selection to the Trash |
| `/`, `S` | Filter, sort apps by name, size or install date |
| `o`, `u` | Reveal in Finder, restore a history entry |
| `s`, `R` | Include system locations, rescan |
| `?`, `q` | All keys, quit |

## Files

cleaner keeps its Delete History in `~/.config/cleaner/trash-history.json`. Until that file exists, it reads the history of the BetterCleaner app, so earlier removals stay restorable. No telemetry, no account.

## Development

```sh
cargo run
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

To make a release, set `version` in `Cargo.toml` and push the tag `v<version>`. The release workflow builds the macOS binaries and publishes them as the latest release.

© 2026 Artur Rok. All rights reserved.
