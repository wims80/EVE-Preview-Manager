# eve-preview-manager-git PKGBUILD

Arch Linux PKGBUILD for the [wims80 fork](https://github.com/wims80/EVE-Preview-Manager)
of EVE Preview Manager. It builds the latest commit on `main`.

```bash
git clone -b pkgbuild --single-branch https://github.com/wims80/EVE-Preview-Manager.git eve-preview-manager-git
cd eve-preview-manager-git
makepkg -si
```

- Replaces the AUR `eve-preview-manager` package; answer `y` when pacman asks.
- Update by running `makepkg -si` again here. `git pull` is only needed when
  the PKGBUILD itself changes.
- ESI (implant reminder) config is read at build time from
  `~/.config/eve-preview-manager/esi.env`. See the comments at the top of the
  PKGBUILD. Without it, ESI is disabled.
