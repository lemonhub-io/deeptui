#!/usr/bin/env bash
# DeepTUI installer — one command to a working `deeptui`.
#
#   curl -fsSL https://raw.githubusercontent.com/lemonhub-io/deeptui/master/install.sh | bash
#
# Downloads the release binary for this platform, verifies its SHA-256 against
# the release's SHA256SUMS.txt, smoke-tests it, then installs it atomically.
# Aborts before touching an existing install if any step fails.
#
#   install.sh --from-source     build from source instead (needs cargo)
#   install.sh --uninstall       remove the installed binary
#   install.sh --help            all options
set -euo pipefail

REPO="lemonhub-io/deeptui"
BIN="deeptui"
INSTALLER_VERSION="0.1.1-beta"
REQUIRED_TAG="v0.1.1-beta"
RELEASE_BASE="https://github.com/$REPO/releases"

# Flags / env defaults. DEEPTUI_VERSION pins the release tag; DEEPTUI_INSTALL_DIR
# overrides the target directory (same as --bin-dir).
tag="${DEEPTUI_VERSION:-$REQUIRED_TAG}"
bin_dir="${DEEPTUI_INSTALL_DIR:-}"
prefix=""
target=""
target_explicit=0
from_source=0
source_dir=""
modify_path=0
verify=1
dry_run=0
uninstall=0

# ---------------------------------------------------------------- output -----

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != "dumb" ]; then
  c_bold=$'\033[1m' c_dim=$'\033[2m' c_green=$'\033[32m'
  c_yellow=$'\033[33m' c_red=$'\033[31m' c_off=$'\033[0m'
else
  c_bold='' c_dim='' c_green='' c_yellow='' c_red='' c_off=''
fi

step() { printf '%s▸%s %s\n' "$c_dim" "$c_off" "$*"; }
ok() { printf '%s✓%s %s\n' "$c_green" "$c_off" "$*"; }
warn() { printf '%swarning:%s %s\n' "$c_yellow" "$c_off" "$*" >&2; }
die() {
  printf '%serror:%s %s\n' "$c_red" "$c_off" "$*" >&2
  exit 1
}

usage() {
  cat <<EOF
${c_bold}DeepTUI installer${c_off} $INSTALLER_VERSION

Usage: install.sh [options]

Options:
  -t, --tag TAG        Required release (default: v0.1.1-beta; retired tags are rejected)
  -b, --bin-dir DIR    Install the binary into DIR
      --prefix DIR     Install into DIR/bin (default: ~/.local, or /usr/local as root)
      --target TRIPLE  Force a release target triple (e.g. x86_64-unknown-linux-gnu)
      --from-source    Build with cargo instead of downloading a binary
      --source-dir DIR Build from an existing checkout instead of cloning
      --modify-path    Append the install dir to your shell rc file if missing
      --no-verify      Skip the SHA-256 checksum verification (not recommended)
  -n, --dry-run        Print what would happen, then exit
  -u, --uninstall      Remove the installed binary
  -V, --version        Print the installer version
  -h, --help           This help

Environment:
  DEEPTUI_VERSION       Same as --tag
  DEEPTUI_INSTALL_DIR   Same as --bin-dir
  NO_COLOR             Disable coloured output

Prebuilt targets: linux x86_64 (static musl) / aarch64, macOS x86_64 / arm64,
Windows x86_64 (.zip from the releases page). Anything else: --from-source.
EOF
}

# ------------------------------------------------------------- utilities -----

have() { command -v "$1" >/dev/null 2>&1; }

fetch_stdout() {
  if have curl; then
    curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 --connect-timeout 15 "$1"
  elif have wget; then
    wget -q --https-only --tries=3 -O - "$1"
  else
    die "curl or wget is required"
  fi
}

download() { # url dest
  if have curl; then
    curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 --connect-timeout 15 -o "$2" "$1"
  elif have wget; then
    wget -q --https-only --tries=3 -O "$2" "$1"
  else
    die "curl or wget is required"
  fi
}

# 0 when the URL resolves (any 2xx), 1 otherwise.
url_exists() {
  if have curl; then
    curl -fsIL --proto '=https' --connect-timeout 15 -o /dev/null "$1" 2>/dev/null
  elif have wget; then
    wget -q --spider --https-only --tries=1 -O /dev/null "$1" 2>/dev/null
  else
    return 1
  fi
}

sha256_of() {
  if have sha256sum; then
    sha256sum "$1" | awk '{print $1}'
  elif have shasum; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif have openssl; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    printf ''
  fi
}

expand_tilde() {
  # NB: bash tilde-expands case *patterns*, and shellcheck flags quoted '~'
  # as an unexpanded path, so compare against a variable instead.
  local tilde='~'
  if [ "$1" = "$tilde" ]; then
    printf '%s' "$HOME"
  elif [ "${1:0:2}" = "${tilde}/" ]; then
    printf '%s' "$HOME/${1:2}"
  else
    printf '%s' "$1"
  fi
}

# --------------------------------------------------------------- platform ----

# Prebuilt binaries are picked for the host; `--target` overrides.
detect_platform() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux) os_name=linux ;;
    Darwin) os_name=macos ;;
    MINGW* | MSYS* | CYGWIN*) os_name=windows ;;
    FreeBSD) os_name=freebsd ;;
    *) os_name=unknown ;;
  esac

  # An Apple Silicon Mac running Rosetta reports x86_64: prefer the native build.
  if [ "$os_name" = macos ] && [ "$arch" = x86_64 ] &&
    [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
    arch=arm64
  fi

  case "$arch" in
    x86_64 | amd64) cpu_arch=x86_64 ;;
    arm64 | aarch64) cpu_arch=aarch64 ;;
    armv7l | armv6l) cpu_arch=arm ;;
    i386 | i686) cpu_arch=i686 ;;
    *) cpu_arch="$arch" ;;
  esac

  if [ "$target_explicit" -eq 0 ]; then
    case "$os_name:$cpu_arch" in
      linux:x86_64) target=x86_64-unknown-linux-musl ;; # static: no glibc floor
      linux:aarch64) target=aarch64-unknown-linux-gnu ;;
      macos:x86_64) target=x86_64-apple-darwin ;;
      macos:aarch64) target=aarch64-apple-darwin ;;
      *) target="" ;;
    esac
  fi
}

# Release binaries target x86-64-v3 (AVX2/BMI2/FMA). On an older CPU they die
# with SIGILL, so build locally for x86-64-v2 instead.
has_avx2() {
  cpuinfo="${DEEPTUI_INSTALL_CPUINFO:-/proc/cpuinfo}" # override: odd containers, tests
  [ -r "$cpuinfo" ] || return 0
  grep -qw avx2 "$cpuinfo"
}

# ------------------------------------------------------------ resolution -----

# The old ragent version sequence was retired. GitHub's /latest endpoint
# excludes prereleases, so resolve this supported beta explicitly.
resolve_tag() {
  case "$tag" in
    latest) tag="$REQUIRED_TAG" ;;
    v*) ;;
    *) tag="v$tag" ;;
  esac
  [ "$tag" = "$REQUIRED_TAG" ] || die "$tag is unsupported. All previous versions are retired; install $REQUIRED_TAG."
}

asset_name() {
  case "$target" in
    *windows*) printf '%s-%s-%s.zip' "$BIN" "$tag" "$target" ;;
    *) printf '%s-%s-%s.tar.gz' "$BIN" "$tag" "$target" ;;
  esac
}

# ------------------------------------------------------------- integrity -----

verify_asset() { # file, asset name
  if [ "$verify" -eq 0 ]; then
    warn "skipping checksum verification (--no-verify)"
    return 0
  fi
  if ! download "$RELEASE_BASE/download/$tag/SHA256SUMS.txt" "$tmp/SHA256SUMS.txt" 2>/dev/null; then
    die "SHA256SUMS.txt is unavailable for $tag — refusing an unverified download"
  fi
  want="$(sed -n "s/^\([0-9a-fA-F]\{64\}\)[[:space:]]\{1,\}$2\$/\1/p" "$tmp/SHA256SUMS.txt" | head -1)"
  if [ -z "$want" ]; then
    die "no checksum listed for $2 — refusing an unverified download"
  fi
  got="$(sha256_of "$1")"
  if [ -z "$got" ]; then
    die "no sha256sum/shasum/openssl available — cannot verify the download"
  fi
  if [ "$got" != "$want" ]; then
    die "checksum mismatch for $2
  expected $want
  got      $got
refusing to install an unverified binary (pre-existing install left untouched)"
  fi
  ok "checksum verified ($(printf '%.8s' "$got")…)"
}

# ------------------------------------------------------------ installing -----

smoke_test() { # rejects binaries that cannot run on this machine at all
  if out="$("$1" --version 2>&1)"; then
    case "$out" in
      "$BIN ${tag#v}") ok "smoke test: $out" ;;
      *) die "expected $BIN ${tag#v}, received: $out — refusing to install" ;;
    esac
    return 0
  else
    status=$?
  fi
  if [ "$status" -ge 128 ]; then
    warn "$(basename "$1") died from signal $((status - 128)) on this machine"
    warn "release binaries need x86-64-v3 (AVX2/BMI2/FMA, Haswell 2013+ / Excavator 2015+)"
    die "re-run with --from-source to build a binary for this CPU"
  fi
  die "the downloaded binary failed to run (exit $status): $out"
}

install_bin() { # candidate binary path
  cand="$1"
  smoke_test "$cand"
  [ -n "$bin_dir" ] || die "internal: no install directory resolved"

  dst="$bin_dir/$BIN"
  mkdir -p "$bin_dir" 2>/dev/null || die "cannot create $bin_dir"
  if [ -e "$dst" ] && [ ! -w "$dst" ]; then
    die "$dst is not writable — re-run with --prefix \"\$HOME/.local\", or fix its permissions"
  fi
  [ -w "$bin_dir" ] || die "$bin_dir is not writable — re-run with --prefix \"\$HOME/.local\""

  old=""
  if [ -x "$dst" ]; then old="$("$dst" --version 2>/dev/null)" || old=""; fi

  # Copy next to the target, then rename: an interrupted install can never
  # leave a half-written deeptui behind.
  tmpdst="$bin_dir/.$BIN.new.$$"
  if ! install -m 755 "$cand" "$tmpdst" 2>/dev/null; then
    cp "$cand" "$tmpdst" && chmod 755 "$tmpdst"
  fi
  mv -f "$tmpdst" "$dst"
  ok "installed $dst"

  if [ -n "$old" ]; then
    new="$("$dst" --version 2>/dev/null || true)"
    if [ "$old" = "$new" ]; then
      step "already at $new — reinstalled"
    else
      step "upgraded: $old → $new"
    fi
  fi

  # A quarantined binary is blocked by Gatekeeper on first launch.
  if [ "$os_name" = macos ] && have xattr &&
    [ -n "$(xattr "$dst" 2>/dev/null | grep -x com.apple.quarantine || true)" ]; then
    xattr -d com.apple.quarantine "$dst" 2>/dev/null || true
  fi
}

# ------------------------------------------------------------ migration ------

copy_legacy_file() { # old path, new path; an existing destination always wins
  local old_file="$1" new_file="$2" copy_file
  [ -f "$old_file" ] && [ ! -e "$new_file" ] || return 0
  mkdir -p "$(dirname "$new_file")"
  copy_file="$(mktemp "$(dirname "$new_file")/.migrate.XXXXXX")"
  chmod 600 "$copy_file"
  if ! cp "$old_file" "$copy_file"; then
    rm -f "$copy_file"
    die "could not migrate $old_file"
  fi
  # A hard link creates the destination atomically without replacing a config
  # another process created while we copied. Both paths are in the same dir.
  if ln "$copy_file" "$new_file" 2>/dev/null; then
    ok "migrated $old_file → $new_file"
  elif [ ! -e "$new_file" ]; then
    rm -f "$copy_file"
    die "could not migrate $old_file to $new_file"
  fi
  rm -f "$copy_file"
}

# Match the native directories used by the Rust dirs crate. macOS uses
# Application Support for both config and data, rather than XDG paths.
legacy_root() { # platform, user home, XDG/default path
  case "$1" in
    macos) printf '%s/Library/Application Support\n' "$2" ;;
    *) printf '%s\n' "$3" ;;
  esac
}

migrate_legacy() {
  local config_root data_root
  config_root="$(legacy_root "$os_name" "$HOME" "${XDG_CONFIG_HOME:-$HOME/.config}")"
  data_root="$(legacy_root "$os_name" "$HOME" "${XDG_DATA_HOME:-$HOME/.local/share}")"
  copy_legacy_file "$config_root/ragent/config.toml" "$config_root/deeptui/config.toml"
  copy_legacy_file "$data_root/ragent/stats.tsv" "$data_root/deeptui/stats.tsv"

  local legacy="$bin_dir/ragent" legacy_version shim
  [ -x "$legacy" ] || return 0
  legacy_version="$("$legacy" --version 2>/dev/null || true)"
  case "$legacy_version" in
    "ragent 0.1.0"|"ragent 0.2.0"|"ragent 0.3.0"|"ragent 0.4.0"|"ragent 0.5.0"|"ragent 0.6.0") ;;
    *) return 0 ;;
  esac
  shim="$(mktemp "$bin_dir/.ragent.migrate.XXXXXX")"
  cat > "$shim" <<'SH'
#!/bin/sh
# DeepTUI legacy launcher: the retired ragent binary has been replaced.
[ -n "${DEEPTUI_API_KEY:-}" ] || export DEEPTUI_API_KEY="${RAGENT_API_KEY:-}"
[ -n "${DEEPTUI_BASE_URL:-}" ] || export DEEPTUI_BASE_URL="${RAGENT_BASE_URL:-}"
[ -n "${DEEPTUI_MODEL:-}" ] || export DEEPTUI_MODEL="${RAGENT_MODEL:-}"
[ -n "${DEEPTUI_MAP_BYTES:-}" ] || export DEEPTUI_MAP_BYTES="${RAGENT_MAP_BYTES:-}"
[ -n "${DEEPTUI_STATS_FILE:-}" ] || export DEEPTUI_STATS_FILE="${RAGENT_STATS_FILE:-}"
[ -n "${DEEPTUI_NO_UPDATE_CHECK:-}" ] || export DEEPTUI_NO_UPDATE_CHECK="${RAGENT_NO_UPDATE_CHECK:-}"
entry_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
exec "$entry_dir/deeptui" "$@"
SH
  chmod 755 "$shim"
  mv -f "$shim" "$legacy"
  ok "replaced $legacy_version with a launcher for $BIN ${REQUIRED_TAG#v}"
}

# ---------------------------------------------------------------- source -----

build_from_source() {
  have cargo || die "cargo not found — install Rust (https://rustup.rs) and re-run with --from-source"
  src="$source_dir"
  if [ -z "$src" ]; then
    if [ -n "$script_dir" ] && [ -f "$script_dir/Cargo.toml" ]; then
      src="$script_dir"
      step "using the checkout at $src"
    else
      have git || die "git is required to fetch the source (or point --source-dir at a checkout)"
      src="$tmp/src"
      step "cloning $REPO at $tag"
      git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$tag" \
        "https://github.com/$REPO.git" "$src"
    fi
  fi
  [ -f "$src/Cargo.toml" ] || die "no Cargo.toml in $src"

  host="$(rustc -vV | sed -n 's/^host: //p')"
  key="$(printf '%s' "$host" | tr 'a-z-' 'A-Z_')"

  # .cargo/config.toml hardcodes mold and x86-64-v3; override both where this
  # machine cannot satisfy them, otherwise the build fails or produces a
  # binary that dies with SIGILL.
  flags=""
  case "$host" in
    x86_64-*)
      if has_avx2; then flags="-C target-cpu=x86-64-v3"; else
        step "no AVX2 on this CPU — building for x86-64-v2"
        flags="-C target-cpu=x86-64-v2"
      fi
      ;;
  esac
  if have mold; then
    flags="${flags:+$flags }-C link-arg=-fuse-ld=mold"
  elif [ "$os_name" = linux ]; then
    step "mold not found — linking with the default linker"
  fi

  linker=""
  case "$host" in
    x86_64-unknown-linux-gnu | x86_64-unknown-linux-musl)
      have gcc || linker=cc
      ;;
    aarch64-unknown-linux-gnu) # config points at the cross gcc, absent on native arm64
      have aarch64-linux-gnu-gcc || linker=cc
      ;;
  esac

  step "cargo build --release --locked (this takes a minute or two)"
  if [ -n "$linker" ]; then
    env "CARGO_TARGET_${key}_RUSTFLAGS=${flags}" \
      "CARGO_TARGET_${key}_LINKER=${linker}" \
      cargo build --release --locked --manifest-path "$src/Cargo.toml"
  else
    env "CARGO_TARGET_${key}_RUSTFLAGS=${flags}" \
      cargo build --release --locked --manifest-path "$src/Cargo.toml"
  fi

  built="$src/target/release/$BIN"
  [ -x "$built" ] || built="$src/target/$host/release/$BIN"
  [ -x "$built" ] || die "build finished but $BIN is not at $src/target/release/$BIN"

  install_bin "$built"
  step "source build left in $src/target/release/ (rm -rf to reclaim space)"
}

# ------------------------------------------------------------------ PATH -----

add_to_rc() {
  shell_name="$(basename "${SHELL:-sh}")"
  case "$shell_name" in
    zsh) rc="${ZDOTDIR:-$HOME}/.zshrc"; line="export PATH=\"$bin_dir:\$PATH\"" ;;
    bash)
      if [ "$os_name" = macos ]; then rc="$HOME/.bash_profile"; else rc="$HOME/.bashrc"; fi
      line="export PATH=\"$bin_dir:\$PATH\""
      ;;
    fish) rc="$HOME/.config/fish/config.fish"; line="fish_add_path $bin_dir" ;;
    *)
      warn "unknown shell ($shell_name) — add $bin_dir to PATH manually"
      return 1
      ;;
  esac
  if [ -z "${HOME:-}" ]; then
    warn "HOME is unset — add $bin_dir to PATH manually"
    return 1
  fi
  if [ -f "$rc" ] && grep -qF "$line" "$rc"; then
    step "PATH already set in $rc"
    return 0
  fi
  mkdir -p "$(dirname "$rc")" 2>/dev/null || true
  if ! printf '\n# added by the %s installer\n%s\n' "$BIN" "$line" >>"$rc"; then
    warn "cannot write $rc"
    return 1
  fi
  ok "added $bin_dir to PATH in $rc (new shells pick it up)"
}

path_hint() {
  case ":$PATH:" in *":$bin_dir:"*) return 0 ;; esac
  if [ "$modify_path" -eq 1 ] && add_to_rc; then
    return 0
  fi
  warn "$bin_dir is not in your PATH"
  # shellcheck disable=SC2016  # $PATH must stay literal in this hint
  printf '  add it with:%s\n    export PATH="%s:$PATH"%s\n' "$c_dim" "$bin_dir" "$c_off"
  if [ "$modify_path" -eq 0 ]; then
    printf '  or re-run with --modify-path to edit your shell rc file\n'
  fi
}

# ----------------------------------------------------------------- main ------

do_uninstall() {
  dst="$bin_dir/$BIN"
  [ -e "$dst" ] || die "no $BIN at $dst — pass --prefix/--bin-dir if it lives elsewhere"
  rm -f "$dst"
  ok "removed $dst"
  if [ -f "$bin_dir/ragent" ] && grep -qF '# DeepTUI legacy launcher:' "$bin_dir/ragent"; then
    rm -f "$bin_dir/ragent"
    ok "removed the legacy ragent launcher"
  fi
  if [ -n "${HOME:-}" ]; then
    step "left in place: ${XDG_CONFIG_HOME:-$HOME/.config}/deeptui (config)"
    step "                ${XDG_DATA_HOME:-$HOME/.local/share}/deeptui (usage stats)"
  fi
}

main() {
  while [ $# -gt 0 ]; do
    case "$1" in
      -h | --help)
        usage
        exit 0
        ;;
      -V | --version)
        printf 'DeepTUI installer %s\n' "$INSTALLER_VERSION"
        exit 0
        ;;
      -t | --tag | --tag=*)
        case "$1" in --tag=*) tag="${1#*=}" ;; *)
          [ $# -ge 2 ] || die "--tag needs a value"
          tag="$2"
          shift
          ;;
        esac
        ;;
      -b | --bin-dir | --bin-dir=*)
        case "$1" in --bin-dir=*) bin_dir="${1#*=}" ;; *)
          [ $# -ge 2 ] || die "--bin-dir needs a value"
          bin_dir="$2"
          shift
          ;;
        esac
        ;;
      --prefix | --prefix=*)
        case "$1" in --prefix=*) prefix="${1#*=}" ;; *)
          [ $# -ge 2 ] || die "--prefix needs a value"
          prefix="$2"
          shift
          ;;
        esac
        ;;
      --target | --target=*)
        case "$1" in --target=*) target="${1#*=}" ;; *)
          [ $# -ge 2 ] || die "--target needs a value"
          target="$2"
          shift
          ;;
        esac
        target_explicit=1
        ;;
      --source-dir | --source-dir=*)
        case "$1" in --source-dir=*) source_dir="${1#*=}" ;; *)
          [ $# -ge 2 ] || die "--source-dir needs a value"
          source_dir="$2"
          shift
          ;;
        esac
        from_source=1
        ;;
      --from-source) from_source=1 ;;
      --modify-path) modify_path=1 ;;
      --no-verify) verify=0 ;;
      -n | --dry-run) dry_run=1 ;;
      -u | --uninstall) uninstall=1 ;;
      *) die "unknown option: $1 (try --help)" ;;
    esac
    shift
  done

  # Root installs system-wide; everyone else stays inside $HOME (no sudo needed).
  if [ -z "$bin_dir" ]; then
    if [ -z "$prefix" ]; then
      if [ "$(id -u)" -eq 0 ]; then prefix=/usr/local; else prefix="$HOME/.local"; fi
    fi
    bin_dir="$(expand_tilde "$prefix")/bin"
  else
    bin_dir="$(expand_tilde "$bin_dir")"
  fi

  if [ "$uninstall" -eq 1 ]; then
    do_uninstall
    exit 0
  fi

  detect_platform

  if [ "$os_name" = windows ]; then
    die "Windows: grab deeptui-<tag>-x86_64-pc-windows-msvc.zip from $RELEASE_BASE/tag/$REQUIRED_TAG and put deeptui.exe on PATH"
  fi
  if [ -z "$target" ] && [ "$from_source" -eq 0 ]; then
    if [ -z "$source_dir" ] && [ -z "$script_dir" ]; then
      die "no prebuilt binary for $os_name/$cpu_arch — re-run with --from-source (needs cargo)"
    fi
    warn "no prebuilt binary for $os_name/$cpu_arch — building from source instead"
    from_source=1
  fi

  resolve_tag

  # Old x86 CPUs: the release binary would SIGILL, so prefer a local build.
  # An explicit --target is a deliberate override: warn, don't switch.
  if [ "$from_source" -eq 0 ] && [ "$cpu_arch" = x86_64 ] && ! has_avx2; then
    if [ "$target_explicit" -eq 1 ]; then
      warn "this CPU has no AVX2 — $target needs x86-64-v3 and may die with SIGILL"
    elif have cargo; then
      warn "this CPU has no AVX2 — release binaries need x86-64-v3; building from source instead"
      warn "(pass --target $target to try the prebuilt anyway)"
      from_source=1
    else
      die "this CPU has no AVX2, so the prebuilt binary cannot run — install Rust (https://rustup.rs) and re-run with --from-source"
    fi
  fi

  if [ "$dry_run" -eq 1 ]; then
    printf '%splan%s\n' "$c_bold" "$c_off"
    printf '  platform    %s/%s\n' "$os_name" "$cpu_arch"
    printf '  release     %s\n' "$tag"
    if [ "$from_source" -eq 1 ]; then
      printf '  mode        source build (cargo)\n'
      printf '  source      %s\n' "${source_dir:-${script_dir:-(clone $REPO at $tag)}}"
      printf '  install to  %s\n' "$bin_dir/$BIN"
    else
      printf '  target      %s\n' "$target"
      printf '  asset       %s\n' "$(asset_name)"
      printf '  url         %s/download/%s/%s\n' "$RELEASE_BASE" "$tag" "$(asset_name)"
      printf '  verify      %s\n' "$([ "$verify" -eq 1 ] && echo 'sha256 (SHA256SUMS.txt)' || echo 'skipped (--no-verify)')"
      printf '  install to  %s\n' "$bin_dir/$BIN"
    fi
    printf '  path        %s\n' "$(case ":$PATH:" in *":$bin_dir:"*) echo 'already on PATH' ;; *) echo "not on PATH ($bin_dir)" ;; esac)"
    exit 0
  fi

  if [ "$from_source" -eq 1 ]; then
    build_from_source
  else
    url="$RELEASE_BASE/download/$tag/$(asset_name)"
    if ! url_exists "$url"; then
      die "no asset $(asset_name) in $tag — re-run with --from-source, or --target to pick another build"
    fi
    step "downloading $(asset_name)"
    download "$url" "$tmp/$(asset_name)"
    verify_asset "$tmp/$(asset_name)" "$(asset_name)"

    mkdir -p "$tmp/x"
    case "$(asset_name)" in
      *.zip) have unzip || die "unzip is required to extract $(asset_name)"; unzip -q "$tmp/$(asset_name)" -d "$tmp/x" ;;
      *) tar -xzf "$tmp/$(asset_name)" -C "$tmp/x" ;;
    esac
    cand="$(find "$tmp/x" -type f \( -name "$BIN" -o -name "$BIN.exe" \) -print -quit)"
    [ -n "$cand" ] || die "$(asset_name) does not contain a $BIN binary"
    chmod 755 "$cand"
    install_bin "$cand"
  fi

  migrate_legacy
  path_hint
  printf '\n'
  printf '%s%s is ready.%s Run it with: %s%s%s\n' "$c_bold" "$BIN" "$c_off" "$c_bold" "$bin_dir/$BIN" "$c_off"
  printf 'First run: set an API key (%sDEEPTUI_API_KEY=sk-…%s) or run %s%s --init%s.\n' \
    "$c_dim" "$c_off" "$c_dim" "$BIN" "$c_off"
  printf 'Later: %s%s --self-update%s swaps in the newest release in place.\n' \
    "$c_dim" "$BIN" "$c_off"
}

# Clean up the scratch dir on any exit path (including failures).
tmp=""
cleanup() {
  if [ -n "$tmp" ]; then rm -rf "$tmp"; fi
}
trap cleanup EXIT

if [ -z "${BASH_VERSION:-}" ]; then
  printf 'error: this installer needs bash (run: curl … | bash)\n' >&2
  exit 1
fi

# Directory of this script when it is a real file; empty when piped.
script_dir=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/deeptui-install.XXXXXX")"
main "$@"
