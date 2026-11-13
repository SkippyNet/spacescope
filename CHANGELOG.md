# Changelog

Versions follow [semantic versioning](https://semver.org). A release is made
by setting `version` in `Cargo.toml`, adding a section here, and pushing a
matching `vX.Y.Z` tag; CI checks the three agree, builds both platforms and
publishes the release.

## 1.1.1

- Licence changed from MIT to the Apache License 2.0, which adds an explicit
  patent grant and contribution terms. Version 1.1.0 remains under MIT.
- The website lists every release with its notes, downloads and sizes, and
  offers the download for your own system first.
- README and website explain how the scanner differs from walking the disk
  folder by folder.
- The Windows zip includes the licence and NOTICE files.

## 1.1.0

- Windows: when run as administrator, drives and their top-level folders are
  read straight from the NTFS master file table. A 712 GB system drive with
  1.3 million files takes about 2.4 s instead of 8–10 s, with hard links
  counted once and no unreadable folders. **More → Restart as administrator**
  switches to it.
- Live updates: after a scan the folder is watched and the map follows
  changes on disk. Folders keep their place while their content changes.
- The status bar shows how the scan was done and whether the map is live.
- `spacescope --version`, `sscan --version`, and `sscan --no-mft`.
- Release downloads have stable names (`SpaceScope-windows-x64.zip`,
  `spacescope-amd64.deb`).

## 1.0.0

- Internal first version, not published: parallel scanner for Windows and Linux, live treemap with
  animated zoom, filters, on-disk and logical sizes, free-space tile, file
  actions, CSV export, dark and light themes, `sscan` command-line tool.
