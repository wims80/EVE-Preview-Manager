# ESI implant-reminder release configuration

The implant reminder is inactive unless all four build variables below are
set at compile time; a build without them succeeds with ESI disabled. Keep the
values out of the repository: the Arch package reads them from a local file
(see the PKGBUILD on the `pkgbuild` branch).
Values must describe an EVE Preview Manager-owned native/public SSO
application; never borrow another application's client ID or add a secret.

- `EPM_ESI_CLIENT_ID`: registered public/native client ID
- `EPM_ESI_CALLBACK`: registered fixed loopback callback, for example
  `http://127.0.0.1:28761/callback`
- `EPM_MAINTAINER_CONTACT`: reviewed, monitored maintainer contact for the ESI
  User-Agent
- `EPM_ESI_COMPATIBILITY_DATE`: fixed API compatibility date reviewed for the
  release

The registered scope list must be exactly
`esi-location.read_location.v1 esi-clones.read_implants.v1`. Authorization uses
code flow with PKCE-S256 and no client secret. Before publishing, verify the
registered callback and client ID, run the mocked conformance suite, and record
an independent security/ESI review.

## Packaging

Native and Nix packages need CA certificates, D-Bus Secret Service access,
`xdg-open`/the OpenURI portal, and permission to bind the registered loopback
port. The Nix derivation exposes matching arguments (`esiClientId`,
`esiCallback`, `maintainerContact`, and `esiCompatibilityDate`) and includes
D-Bus plus `xdg-utils` at runtime.

The external Flatpak manifest is not stored in this repository. Its release
must provide:

```yaml
finish-args:
  - --share=network
  - --talk-name=org.freedesktop.secrets
  - --talk-name=org.freedesktop.portal.Desktop
```

The Flatpak build must inject the same four reviewed build variables. Test
portal browser opening, loopback callback delivery, and Secret Service storage
inside the final sandbox. Do not add filesystem token storage as a fallback.
