# Fork naming, attribution, and release guidance

This repository is intended to become an independently maintained continuation
of [EVE Preview Manager](https://github.com/h0lylag/EVE-Preview-Manager).

For an independent continuation, the cleanest etiquette is to rename the
application, preserve upstream credit, and clearly distinguish its releases and
support channels. Renaming is not required by the MIT license, but it avoids
users confusing this fork with the original project.

## Attribution and licensing

- Keep the original MIT copyright notice in `LICENSE`. Do not replace
  `Copyright (c) 2025 h0lylag`.
- A separate copyright line may be added for new contributions made to this
  fork.
- Keep the Git history. It contains valuable authorship information, although
  it does not replace the license notice required with copies or substantial
  portions of the software.
- Add a prominent lineage statement near the top of the eventual renamed
  README, for example:

  > **Project lineage:** NewName is an independently maintained fork of
  > [EVE Preview Manager](https://github.com/h0lylag/EVE-Preview-Manager),
  > originally created by h0lylag. It is not affiliated with or endorsed by the
  > upstream project. This fork is maintained by Wim Sjøholm.

The relevant license reference is the
[MIT License](https://opensource.org/license/mit).

## Repository relationship

Keep this repository's URL as `origin` and configure the original repository as
`upstream` for comparison and synchronization:

```bash
git remote add upstream https://github.com/h0lylag/EVE-Preview-Manager.git
```

See GitHub's
[documentation for configuring an upstream remote](https://docs.github.com/en/pull-requests/how-tos/work-with-forks/configuring-a-remote-repository-for-a-fork).

## Naming recommendation

Choose a new public product and repository name. Avoid names containing
"Official," "Next," or similar wording that could imply endorsement. A suffix
such as "Community Edition" is clearer, but a genuinely distinct name is best.

A complete rename must eventually cover more than the README:

- Display name and repository name
- Cargo package, CLI, and binary name
- Desktop and AppStream IDs
- Icon filenames and window classes
- Configuration directory
- Keyring service identifier
- Nix and FlakeHub publishing namespace
- ESI application registration and user-agent

The current `eve-preview-manager`, `com.evepreview.manager`, configuration path,
and keyring service could collide with an installation of the original
application. If existing fork users need their configuration preserved, design
an explicit one-time migration when changing these identifiers.

## References currently needing attention

This repository still directs users toward infrastructure controlled by the
upstream maintainer:

- `README.md` contains the upstream website, Discord, Flathub, AUR, FlakeHub,
  screenshots, releases, and clone instructions.
- `assets/com.evepreview.manager.metainfo.xml` contains the original developer,
  website, issue tracker, and repository.
- `default.nix` contains the original maintainer, homepage, and changelog.
- `src/daemon/esi/worker.rs` embeds the upstream repository in the ESI
  user-agent.
- `.github/workflows/flakehub-publish-release.yml` targets the original owner's
  FlakeHub namespace.

Remove these references, replace them with resources controlled by this fork,
or explicitly label them as upstream resources. Do not direct fork users to the
upstream issue tracker or Discord for support.

## EVE and CCP attribution

CCP's requirements are separate from the MIT license. The current CCP developer
agreement prohibits implying that an application represents CCP, requires a
proprietary notice covering EVE and CCP marks, and requires the developer to
accept the agreement before creating a third-party application.

- Retain the existing "not affiliated with CCP Games" disclaimer and add an
  equivalent notice to the README or About UI.
- Register and use an ESI application and client ID controlled by this fork.
  Never reuse the upstream maintainer's credentials.
- Review the
  [CCP Developer License Agreement](https://developers.eveonline.com/license-agreement)
  and [developer documentation](https://developers.eveonline.com/docs/resources/license/)
  before publishing.

## Bundled assets requiring review

The upstream MIT license does not automatically establish that every bundled
asset was licensed by its rights holder.

- `assets/README.md` says `decloaked.mp3` lacks confirmed rights-holder
  provenance. Confirm the license or replace the file before a public release.
- `assets/com.evepreview.manager.svg` contains an external SVG Repo reference
  without clear license documentation. Confirm and document its license or
  replace it.
- Preserve the documented attribution and provenance for the EVE Static Data
  Export-derived implant data.

## Practical release checklist

- Select a distinct name and identifier namespace.
- Preserve the original MIT notice and add clear upstream attribution.
- Replace upstream-owned URLs, maintainers, support channels, and publishing
  targets.
- Establish fork-owned ESI credentials, contact details, and user-agent.
- Add the CCP proprietary and non-affiliation notices.
- Resolve or replace assets whose redistribution rights are unclear.
- Decide whether existing configuration and keyring data will be migrated.
- Publish releases and accept issues only through channels controlled by this
  fork.

This document is practical open-source guidance, not formal legal advice.
