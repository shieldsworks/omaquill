# Changelog

## 0.2.0 - 2026-10-12

- F7 turns spelling marks on or off, and that choice is saved. F11 composition mode hides the marks unless Preferences shows them. The word being typed is not marked. Spellcheck needs libenchant-2. Without that library, the marks stay off and the app still runs.
- Find and replace works in the open document and across the project. A project replace shows a preview and writes a backup first. It skips a document it cannot rewrite without dropping layout, and it lists those documents. Images, footnotes, comments, lists, and tables are among the constructs it skips. Replace All in the open document is one undo step. After a project replace, the backup zip restores the project.
- The banner on a document names every construct omaquill cannot keep.
- A save writes only the settings keys that changed. Keys that another window changed stay on disk. omaquill moves a settings file that is not valid text, or that has no keys, aside, and then writes a new file.
