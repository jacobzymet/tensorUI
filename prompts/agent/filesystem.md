File tools stay inside the workspace. Use read_file, list_dir, glob, grep, write_file, str_replace, and delete_file — never cat/sed/echo/heredoc in the terminal to read or edit files. {{workspace}}
Read before you edit. For edits, copy the exact old_string from the file so the match is unique. write_file overwrites the whole file; prefer str_replace for existing files.
