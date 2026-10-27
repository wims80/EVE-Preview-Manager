# Maintainer: Wim Sjøholm <wims80@gmail.com>
# Based on the AUR eve-preview-manager PKGBUILD by h0lylag <h0lylag@gravemind.sh>
#
# ESI / EVE SSO configuration
# ---------------------------
# The ESI implant reminder needs four build-time values that are deliberately
# NOT stored in this repository. build() reads them from:
#
#   ${XDG_CONFIG_HOME:-$HOME/.config}/eve-preview-manager/esi.env
#
# (override the path with EPM_ESI_ENV=/path/to/file makepkg -si). Example:
#
#   EPM_ESI_CLIENT_ID=<client ID of your own app at developers.eveonline.com>
#   EPM_ESI_CALLBACK=http://127.0.0.1:28761/callback
#   EPM_MAINTAINER_CONTACT=<URL or email CCP can reach you at>
#   EPM_ESI_COMPATIBILITY_DATE=2026-09-01
#
# Register a native/public (PKCE) application with the scopes
# "esi-location.read_location.v1 esi-clones.read_implants.v1" and exactly the
# callback above; no client secret is used. Without the file the package still
# builds, with ESI disabled. See ESI-COMPLIANCE.md.

_pkgname=eve-preview-manager
pkgname=eve-preview-manager-git
pkgver=1.8.0.r0.g0000000
pkgrel=1
pkgdesc="EVE Online window preview manager (wims80 fork, git)"
arch=('x86_64')
url="https://github.com/wims80/EVE-Preview-Manager"
license=('MIT')
# !lto: makepkg's LTO flags break linking ring's C objects with rust-lld.
options=('!debug' '!lto')
depends=('fontconfig' 'libx11' 'libxcb' 'libxkbcommon' 'libxkbcommon-x11' 'hicolor-icon-theme' 'libxcursor' 'libxrandr' 'libxi'
         'alsa-lib' 'dbus' 'xdg-utils' 'ca-certificates')
makedepends=('rust' 'pkgconf' 'git')
provides=("$_pkgname")
conflicts=("$_pkgname")
source=("$_pkgname::git+https://github.com/wims80/EVE-Preview-Manager.git")
sha256sums=('SKIP')

pkgver() {
  cd "$_pkgname"
  local _ver
  _ver=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
  printf "%s.r%s.g%s" "$_ver" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd "$_pkgname"
  export CARGO_HOME="$srcdir/cargo-home"
  cargo fetch --locked --target "$CARCH-unknown-linux-gnu"
  sed -i "s/^Icon=.*/Icon=$_pkgname/" assets/com.evepreview.manager.desktop
}

build() {
  local _esi_env="${EPM_ESI_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/eve-preview-manager/esi.env}"
  if [[ -f "$_esi_env" ]]; then
    msg2 "Loading ESI config from $_esi_env"
    set -a; source "$_esi_env"; set +a
  else
    warning "No ESI config at $_esi_env; building with ESI disabled"
  fi

  cd "$_pkgname"
  export CARGO_HOME="$srcdir/cargo-home"
  cargo build --frozen --release --all-features
}

package() {
  cd "$_pkgname"
  install -Dm755 "target/release/eve-preview-manager" "$pkgdir/usr/bin/eve-preview-manager"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 assets/com.evepreview.manager.desktop "$pkgdir/usr/share/applications/eve-preview-manager.desktop"
  install -Dm644 assets/com.evepreview.manager.svg "$pkgdir/usr/share/icons/hicolor/scalable/apps/eve-preview-manager.svg"
}
