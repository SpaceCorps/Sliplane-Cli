#!/bin/sh
# Install the Sliplane CLI (https://github.com/SpaceCorps/Sliplane-Cli) without sudo.
#
#   curl -fsSL https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.sh | sh
#
# Usage: sh install.sh [VERSION] [--setup-path] [--non-interactive]
#
# Environment:
#   SLIPLANE_INSTALL_DIR    where to put the binary (default: ~/.sliplane-cli/bin)
#   SLIPLANE_RELEASES_URL   a mirror of the GitHub releases page, for air-gapped installs and tests
set -eu

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

# download fetches one file. Without a mirror it accepts HTTPS only, including redirects from
# GitHub to its asset host.
download() {
  if [ -n "${SLIPLANE_RELEASES_URL:-}" ]; then
    curl --fail --silent --show-error --location \
      --connect-timeout 10 --max-time 300 --retry 2 --output "$2" "$1"
  else
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
      --connect-timeout 10 --max-time 300 --retry 2 --output "$2" "$1"
  fi
}

# executable_version prints the version a binary reports ("sliplane 1.2.0" -> "1.2.0").
executable_version() {
  version_output=$("$1" --version </dev/null 2>/dev/null) || return 1
  printf '%s\n' "$version_output" | awk '
    NF == 2 && $1 == "sliplane" { version=$2; count++ }
    END { if (NR != 1 || count != 1) exit 1; print version }
  '
}

# find_shell_profile sets the profile file and the line that puts the install directory on PATH.
find_shell_profile() {
  quoted_directory=$(printf '%s' "$install_directory" | sed "s/'/'\\\\''/g")
  path_line="export PATH='$quoted_directory':\"\$PATH\""
  config_file=
  case "${SHELL:-}" in
    */zsh) config_file=${ZDOTDIR:-$HOME}/.zshrc ;;
    */bash)
      config_file=$HOME/.bashrc
      if [ ! -f "$config_file" ] && [ -f "$HOME/.bash_profile" ]; then
        config_file=$HOME/.bash_profile
      fi
      ;;
    */fish)
      config_file=${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish
      # fish single quotes only escape backslashes and quotes.
      quoted_directory=$(printf '%s' "$install_directory" | sed -e 's/\\/\\\\/g' -e "s/'/\\\\'/g")
      path_line="set -gx PATH '$quoted_directory' \$PATH"
      ;;
  esac
}

# print_reload_hint shows how to load the updated profile; the installer cannot change the
# environment of the shell that started it.
print_reload_hint() {
  home_prefix=
  profile_display=$config_file
  # The hint must print a literal ~/ for the user to paste, so the tilde is meant to stay unexpanded.
  # shellcheck disable=SC2088
  case "$config_file" in
    "$HOME"/*) home_prefix='~/' profile_display=${config_file#"$HOME"/} ;;
  esac
  case "$profile_display" in
    *[!A-Za-z0-9_./-]*) profile_display="'$(printf '%s' "$profile_display" | sed "s/'/'\\\\''/g")'" ;;
  esac
  printf '\nTo use Sliplane in this terminal:\n\n  source %s%s\n' "$home_prefix" "$profile_display"
}

# configure_path puts the install directory on PATH, asking first unless --setup-path was given.
# Prompts read from the terminal, never from the piped script.
configure_path() {
  case ":$PATH:" in
    *":$install_directory:"*) return ;;
  esac

  if [ -n "$config_file" ] && [ -f "$config_file" ] && grep -Fqx "$path_line" "$config_file"; then
    print_reload_hint
    return
  fi

  response=n
  if [ -n "$config_file" ]; then
    if [ "$setup_path" = true ]; then
      response=y
    elif [ "$non_interactive" = false ] && [ -t 1 ]; then
      printf '\nAdd Sliplane to PATH in %s? (Y/n) ' "$config_file"
      if ! read -r response 2>/dev/null < /dev/tty; then
        response=n
      fi
    fi
  fi
  case "$response" in
    y|Y|yes|Yes|'')
      if [ -n "$config_file" ] && mkdir -p "$(dirname "$config_file")" 2>/dev/null \
        && printf '\n# Added by the Sliplane installer\n%s\n' "$path_line" >> "$config_file"; then
        print_reload_hint
        return
      fi
      printf '\nCould not update %s; Sliplane is installed.\n' "$config_file" >&2
      ;;
  esac
  if [ -n "$config_file" ]; then
    printf '\nAdd the installation directory to your shell PATH. Add this line to %s:\n\n  %s\n' "$config_file" "$path_line"
  else
    printf '\nAdd the installation directory to your shell PATH. For bash/zsh, add this line to your shell profile:\n\n  %s\n' "$path_line"
  fi
  printf '\nRun the line in this terminal too to use Sliplane immediately.\n'
}

# install_sliplane keeps the whole operation in a function so a truncated piped script cannot run it.
install_sliplane() {
  version=
  non_interactive=false
  setup_path=false
  for argument in "$@"; do
    case "$argument" in
      --non-interactive) non_interactive=true ;;
      --setup-path) setup_path=true ;;
      --help|-h)
        printf 'Usage: sh install.sh [VERSION] [--setup-path] [--non-interactive]\n'
        return
        ;;
      -*) fail "Unknown option: $argument. Use --help for usage." ;;
      *)
        [ -z "$version" ] || fail 'Specify only one version. Use --help for usage.'
        version=$argument
        ;;
    esac
  done
  for dependency in curl tar awk; do
    command -v "$dependency" >/dev/null 2>&1 || fail "Install $dependency and try again."
  done

  case "$(uname -s)" in
    Darwin) platform=darwin ;;
    Linux) platform=linux ;;
    MINGW*|MSYS*|CYGWIN*)
      fail 'This installer is for macOS, Linux and WSL. On Windows, run in PowerShell: irm https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.ps1 | iex'
      ;;
    *) fail 'Supported systems are macOS and Linux (including WSL).' ;;
  esac
  machine=$(uname -m)
  # A shell started under Rosetta reports x86_64 on an Apple Silicon Mac; take the native build.
  if [ "$platform" = darwin ] && [ "$machine" = x86_64 ] \
    && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
    machine=arm64
  fi
  case "$platform/$machine" in
    darwin/arm64) target=aarch64-apple-darwin ;;
    darwin/x86_64) target=x86_64-apple-darwin ;;
    linux/x86_64|linux/amd64) target=x86_64-unknown-linux-musl ;;
    linux/aarch64|linux/arm64) target=aarch64-unknown-linux-musl ;;
    *) fail "Supported architectures are x86_64 and ARM64; this machine reports $machine." ;;
  esac

  checksum_tool=sha256sum
  if ! command -v sha256sum >/dev/null 2>&1; then
    checksum_tool=shasum
    command -v shasum >/dev/null 2>&1 || fail 'Install sha256sum or shasum and try again.'
  fi

  releases=${SLIPLANE_RELEASES_URL:-https://github.com/SpaceCorps/Sliplane-Cli/releases}
  version=${version:-latest}
  if [ "$version" = latest ]; then
    release_url=$(curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
      --connect-timeout 10 --max-time 30 --retry 2 --output /dev/null --write-out '%{url_effective}' "$releases/latest") \
      || fail 'Could not find the latest release. Check your connection and try again.'
    case "$release_url" in
      "$releases/tag/"*) version=${release_url##*/} ;;
      *) fail 'GitHub did not return a published release.' ;;
    esac
  fi
  version=${version#v}
  if ! printf '%s\n' "$version" | awk '/^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$/ { valid=1 } END { exit !valid }'; then
    fail 'Use a version such as 1.3.0 or v1.3.0-rc.1.'
  fi

  install_directory=${SLIPLANE_INSTALL_DIR:-"$HOME/.sliplane-cli/bin"}
  case "$install_directory" in
    /*) ;;
    *) fail 'SLIPLANE_INSTALL_DIR must be an absolute path.' ;;
  esac
  destination=$install_directory/sliplane
  if [ -L "$destination" ] || [ -d "$destination" ]; then
    fail "$destination is a symlink or directory. Choose another SLIPLANE_INSTALL_DIR."
  fi

  temporary_directory=$(mktemp -d)
  staged_binary=
  trap 'rm -rf "$temporary_directory"; if [ -n "$staged_binary" ]; then rm -f "$staged_binary"; fi' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  package=sliplane-v$version-$target
  archive=$package.tar.gz
  release=$releases/download/v$version
  printf 'Downloading Sliplane %s (%s)…\n' "$version" "$target"
  download "$release/$archive" "$temporary_directory/$archive" \
    || fail "Could not download $archive. Does release v$version exist for this platform?"
  download "$release/checksums.txt" "$temporary_directory/checksums.txt" \
    || fail "Could not download checksums.txt for v$version. Releases before 1.3.0 have none; a release still being built may need a few minutes."
  expected=$(awk -v file="$archive" '$2 == file { count++; hash=$1 } END { if (count != 1) exit 1; print hash }' "$temporary_directory/checksums.txt") \
    || fail "The release must contain exactly one checksum for $archive."
  if [ "$checksum_tool" = sha256sum ]; then
    actual=$(sha256sum "$temporary_directory/$archive")
  else
    actual=$(shasum -a 256 "$temporary_directory/$archive")
  fi
  actual=${actual%% *}
  [ "$actual" = "$expected" ] || fail 'Checksum verification failed. The existing installation was not changed.'
  tar -xzf "$temporary_directory/$archive" -C "$temporary_directory" "$package/sliplane" \
    || fail 'Could not extract the Sliplane executable.'
  extracted=$temporary_directory/$package/sliplane
  if [ ! -f "$extracted" ] || [ -L "$extracted" ]; then
    fail 'The release does not contain a regular Sliplane executable.'
  fi

  mkdir -p "$install_directory" || fail "Cannot create $install_directory. Choose a writable SLIPLANE_INSTALL_DIR."
  staged_binary=$(mktemp "$install_directory/.sliplane.XXXXXX")
  cp "$extracted" "$staged_binary"
  chmod 755 "$staged_binary"

  verified_version=$(executable_version "$staged_binary") \
    || fail 'The downloaded Sliplane executable could not run or returned invalid version information. The existing installation was not changed.'
  [ "$verified_version" = "$version" ] \
    || fail "The downloaded executable reports version $verified_version instead of $version. The existing installation was not changed."

  first_install=true
  if [ -f "$destination" ]; then
    first_install=false
  fi
  if [ -x "$destination" ] && cmp -s "$staged_binary" "$destination"; then
    printf 'Already up to date: Sliplane %s\n' "$version"
  else
    previous_version=
    if [ "$first_install" = false ]; then
      previous_version=$(executable_version "$destination" || true)
    fi
    mv -f "$staged_binary" "$destination" || fail "Cannot install to $destination."
    staged_binary=
    if [ "$first_install" = true ]; then
      printf 'Installed Sliplane %s to %s\n' "$version" "$destination"
    elif [ -n "$previous_version" ] && [ "$previous_version" != "$version" ]; then
      printf 'Updated Sliplane %s → %s\n' "$previous_version" "$version"
    else
      printf 'Reinstalled Sliplane %s\n' "$version"
    fi
  fi
  find_shell_profile
  configure_path
  current=$(command -v sliplane || true)
  if [ -n "$current" ] && [ "$current" != "$destination" ]; then
    printf '\nYour PATH currently selects %s, not this installation (another Sliplane CLI, such as the official one, uses the same name).\nPut %s first on PATH to use this one.\n' "$current" "$install_directory"
  fi
  if [ "$first_install" = true ]; then
    printf '\nGet started:\n\n  sliplane login\n  sliplane agent-readme   # the manual for an LLM agent\n'
  fi
}

install_sliplane "$@"
