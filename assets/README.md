# Bundled assets

`decloaked.mp3` is copied verbatim from FCtool
(`assets/tts/decloaked.mp3`, commit `e853acf36bbdc260dc427d56eb479f34869237f3`,
https://github.com/wims80/FCtool). Its SHA-256 is
`9bf6a4b147a4bd7ccdd75a10d6bd2bee3d1709f88738b02f40c4869bbf7c2136`.

The feature requester explicitly authorized copying and redistributing this
asset for EVE Preview Manager. FCtool does not currently include a standalone
license file, so maintainers must confirm and document the original recording
or synthesis ownership before a public release.

`esi/implant-types.json` is an English category-20 subset of CCP's EVE Online
Static Data Export. It contains only type ID, type name, and group ID. Regenerate
it reproducibly with `tools/generate-implant-sde.py`, passing the reviewed
generation date and source SHA-256; the generator rejects malformed records,
duplicates, and provenance drift, while the application rejects absent
attribution or source-hash metadata.

The checked-in subset was generated from the English SDE-derived
[`fit_types.json`](https://github.com/wims80/FCtool/blob/4e9c6db013a7e1444d92ce6fb78f008a28a0ab44/fit_types.json)
whose SHA-256 is
`9245d1e013caa7acf418bcda494bdc1e9f4ae22a1e59d71145ac95ceac10dc43`:

```text
python3 tools/generate-implant-sde.py fit_types.json assets/esi/implant-types.json --generation-date 2026-09-11 --source-sha256 9245d1e013caa7acf418bcda494bdc1e9f4ae22a1e59d71145ac95ceac10dc43
```
