# Changelog

## Unreleased

- Backups are private to you. The backups folder and each project's backup folder are at most `0700`, and each backup zip is at most `0600` from the moment it is created. On the next backup, an existing backups folder and that project's backup folder are closed to other users, and that project's existing backup zips and leftover incomplete ones are narrowed to at most `0600`. Symlinks are not followed, and other files and folders are left alone. A backups folder that is a symlink is now refused, and the backup fails with a message naming it. The private copy that Open in Default App makes is created `0600` rather than narrowed after creation.

## 0.2.0 - 2026-10-12

- F7 turns spelling marks on or off, and that choice is saved. F11 composition mode hides the marks unless Preferences shows them. The word being typed is not marked. Spellcheck needs libenchant-2. Without that library, the marks stay off and the app still runs.
- Find and replace works in the open document and across the project. A project replace shows a preview and writes a backup first. It skips a document it cannot rewrite without dropping layout, and it lists those documents. Images, footnotes, comments, lists, and tables are among the constructs it skips. Replace All in the open document is one undo step. After a project replace, the backup zip restores the project.
- The banner on a document names every construct omaquill cannot keep.
- A save keeps other windows' changes to a settings file that already has settings. A damaged settings file is moved aside, then the settings are written. The damaged file is invalid UTF-8, or it is non-empty and has no keys. A missing or empty file is simply written.
