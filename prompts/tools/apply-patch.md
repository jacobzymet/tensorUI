Apply several targeted edits in one call. Pass {"patch":"*** Begin Patch\n...\n*** End Patch"}. Supports *** Add File: path (every content line starts with +), *** Delete File: path, and *** Update File: path with one or more @@ hunks. Hunk lines start with a space for unchanged context, - for removals, or + for additions. Include unique exact context; LF/CRLF differences are handled without changing surrounding line endings. Optional @@ followed by an exact anchor line starts searching after that line. Use *** End of File after a hunk to anchor it at EOF, including insertion-only appends. Optional *** Move to: path immediately after Update File moves the result to a new path. Combine multiple edits to one file under one Update File. All paths and hunks are checked before any write; ambiguous or missing context rejects the patch. Maximum patch 1 MB, 64 file operations, target files 8 MB. Read relevant file pages first; never use whole-file replacement for a small addition.

Example patch string (newlines shown literally):
*** Begin Patch
*** Update File: src/example.js
@@
 const count = 1;
-const label = "old";
+const label = "new";
*** Add File: notes.txt
+Created by this patch.
*** End Patch
