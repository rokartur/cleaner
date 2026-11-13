# cleaner

A terminal UI that uninstalls Mac apps with all their leftovers, cleans junk, finds files of apps you already deleted and manages Homebrew.

```text
 Cleaner              │ Homebrew › Installed  199 packages · 9.5 GB            sort name   / filter
                      │─────────────────────────────────────────────────────────────────────────────
  1 Apps          111 │      NAME                                  VERSION                  SIZE
  2 Junk        14 GB │▌ [ ] bat                                   0.26.1                 5.0 MB   █
  3 Orphans      2122 │  [ ] bettercmdtab@beta                     26.8-beta.10,20261…     11 MB
  4 History        81 │  [ ] bitwarden-cli                         2026.10.1               90 MB
  5 Homebrew          │  [ ] btop                                  1.4.8                  1.5 MB
  ▌ Installed     199 │  [ ] cloudflared                           2026.11.0               26 MB
    Outdated        1 │  [ ] coreutils                             9.12_1                  15 MB
    Search            │  [ ] docker                                29.9.3                  29 MB
    Services      1/3 │  [ ] docker-buildx                         0.39.0                  66 MB
    Taps            9 │  [ ] docker-compose                        5.7.1                   29 MB
    Maintenance       │  [ ] docker-credential-helper              0.9.13                 5.7 MB
                      │  [ ] exiftool                              13.55_1                 59 MB
 disk ━━━━━━━━━━━━━━━ │  [ ] eza                                   0.23.5                 1.7 MB
 236 GB free          │  [ ] fastfetch                             2.71.0                 2.0 MB
────────────────────────────────────────────────────────────────────────────────────────────────────
 space pick  u upgrade  d uninstall  z zap  p pin  / filter  S sort  o homepage  ? help  q quit
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

A release build checks GitHub at each start. When it finds a newer release, it installs it in the background, and the new version runs from the next start. The update channel on the Settings page picks what counts: stable takes releases only, and pre-release also takes betas such as `0.3.0-beta.1`. A beta build on the stable channel moves to the release that follows it. A build from source never updates itself.

## Pages

- **Apps:** open an app to see what it takes on disk, the app plus every file it left behind. Matches by bundle id or full name are selected. Vendor folders and partial name matches stay unselected for you to check.
- **Junk:** user caches, logs, saved state and developer caches. Toolchains and package stores stay unselected. The Cargo Targets group lists the `target` folder of every Rust project in your home folder: select the group to run the equivalent of `cargo clean` in all of them.
- **Orphans:** files of apps you already deleted, graded by confidence. Nothing is preselected.
- **History:** every removal, with `u` to put the files back.
- **Homebrew:** six views in the sidebar. Installed and Outdated list your formulae and casks with their size, the ones you asked for first. Search finds packages to install. Services starts and stops `brew services`, Taps adds and removes taps, and Maintenance runs upgrade, cleanup, autoremove, update and doctor with what each would do now.
- **Settings:** the update channel, stable or pre-release, and the version of this build.

Every removal on the Apps, Junk and Orphans pages goes to the Trash, in one `<app> — <date>` folder. Press `s` to also scan `/Library`, `/etc` and `/var/log`. Root-owned files move with `sudo`, after you confirm and see each path.

Homebrew commands that install, upgrade or remove show the exact `brew` command first. After you confirm with `y`, they run in the terminal, so you see brew's output and its prompts. What brew removes skips Delete History, and most of it is deleted for good.

## Keys

| Key | Action |
| --- | --- |
| `1`-`6`, `tab` | Switch page |
| `↑` `↓`, `j` `k`, `g` `G` | Move, jump to the top or bottom |
| `enter`, `esc` | Open an app or a group, go back |
| `space`, `a` | Select an item or a group, select only the safe items |
| `d` | Move the selection to the Trash |
| `/`, `S` | Filter, sort apps by name, size or install date |
| `o`, `u` | Reveal in Finder, restore a history entry |
| `s`, `R` | Include system locations, rescan |
| `?`, `q` | All keys, quit |

In the Homebrew views:

| Key | Action |
| --- | --- |
| `space`, `a` | Pick a package, pick every package shown |
| `u`, `d`, `z` | Upgrade, uninstall, uninstall a cask with its files in `~/Library` |
| `p`, `o` | Pin or unpin a package, open the homepage |
| `/`, `i` | Search, install the picked results |
| `enter`, `r` | Start or stop a service, restart it |
| `a`, `d`, `o` | Add a tap, remove a tap, open its repository |
| `enter` | Run a maintenance task |

On the Settings page, `enter` switches the update channel and checks for an update, and `u` checks without switching.

## Files

cleaner keeps its Delete History in `~/.config/cleaner/trash-history.json` and its settings in `~/.config/cleaner/settings.json`. Until the history file exists, it reads the history of the BetterCleaner app, so earlier removals stay restorable. No telemetry, no account.

## Development

```sh
cargo run
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

To make a release, set `version` in `Cargo.toml` and push the tag `v<version>`. The release workflow builds the macOS binaries and publishes them as the latest release. A version with a suffix, such as `0.2.0-beta.1`, is published as a pre-release: `install.sh` and the stable channel skip it, and the pre-release channel installs it.

© 2026 Artur Rok. All rights reserved.
