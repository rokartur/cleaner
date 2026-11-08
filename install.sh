#!/bin/sh
# Installs the latest cleaner release: curl -fsSL https://raw.githubusercontent.com/rokartur/cleaner/main/install.sh | sh
set -eu

main() {
  if [ "$(uname -s)" != Darwin ]; then
    echo "cleaner: only macOS is supported." >&2
    exit 1
  fi
  case "$(uname -m)" in
    arm64) arch=aarch64 ;;
    x86_64) arch=x86_64 ;;
    *) echo "cleaner: unsupported CPU $(uname -m)." >&2; exit 1 ;;
  esac

  dir="${CLEANER_INSTALL_DIR:-$HOME/.local/bin}"
  mkdir -p "$dir"
  staged="$dir/.cleaner-install-$$"
  trap 'rm -f "$staged"' EXIT
  curl -fsSL "https://github.com/rokartur/cleaner/releases/latest/download/cleaner-macos-$arch" -o "$staged"
  chmod 755 "$staged"
  mv "$staged" "$dir/cleaner"
  echo "Installed $("$dir/cleaner" --version) to $dir/cleaner"

  case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "Add it to PATH: export PATH=\"$dir:\$PATH\"" ;;
  esac
}

# Runs only once the whole script has arrived, so a cut-off download does nothing.
main
