# Changelog

The plugin and server share one version number. Every change to either one gets a new
version, so Settings > notesync > Status (plugin) and `/api/v1/health` (server) show
whether a fix has reached a device.

## 0.1.1 (2026-11-08)

### Plugin
- **Fixed:** a rename that only changed letter case (`Note.md` → `note.md`) couldn't be
  applied on Windows, because the new name looked like an existing file. The device kept the
  old name and retried the same server changes on every sync without moving past them.
- Settings > notesync > Status now shows the plugin version that Obsidian loaded.

### Docs
- Diagrams of the plugin's parts, the server's modules and a typical deployment.
- Limits, storage use, automatic cleanup and maintenance in the deployment guide.

### Server
- No changes apart from the version number.

## 0.1.0 (2026-11-07)

First release: the server, the Obsidian plugin for Windows and Android, and automated tests.
