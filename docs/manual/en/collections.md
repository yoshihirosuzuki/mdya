# Collections and collection types

Every document in mdya belongs to a collection. When you create a collection, you choose one collection type. The collection type cannot be changed later. To change it, remove the collection and create it again.

| Collection type | `--type` value | Source | To change the documents |
|---|---|---|---|
| directory | `directory` (default) | A directory | Change the files in the directory, then run `mdya update` |
| archive | `archive` | A zip, tar, or gzip-compressed tar file | Re-create the archive, then run `mdya update` |
| raw | `raw` | Document content you pass with `mdya put` | Pass it again with `mdya put` |

## What a collection has

- Collection name: The name that tells collections apart. It consists only of ASCII letters and digits, `-`, and `_`, and is 1 to 64 characters long. It is case-sensitive. You cannot create a collection with the same name as an existing one.
- Collection type: One of the three in the table above.
- Type-specific attributes: For directory and archive collections, the location to ingest. Raw collections have none.
- Description: Text that tells what the collection is about, in addition to its name. It is optional. An AI assistant can read the description through `list_collections` in MCP and use it to decide which collection to search.

## How locations are read

When you give a location for a directory or archive collection, it is read as follows.

- A leading `~` is replaced with your home directory.
- A relative path is resolved against the working directory at the time you run the command. mdya keeps the location as an absolute path. So whichever working directory you ingest from, the same location is ingested.
- When the path is made absolute, path components that are only `.` are removed. `..` components are kept. Symbolic links are not followed.

## Directory

The files in a directory become documents.

- What becomes a document: Files in the directory whose extension is `md`, `markdown` (Markdown), or `pdf` (PDF). The extension is not case-sensitive. Files with other extensions and files with no extension are not made documents. Files and directories whose names start with `.` are treated the same as any others.
- Key: The file's path relative to the directory. The separator is `/` on every OS.
- Document version: Versions are distinguished by the file's last modified time and the document's body.
- Update: Makes the collection's documents match the current contents of the directory. New files are added, changed files are ingested again, and the documents of files that are gone are removed. However, unreadable documents, and the documents in parts of the directory that could not be read while the directory was being read, stay at their previous document version ([Errors and how to fix them: Some documents cannot be read](errors.md#some-documents-cannot-be-read), [A source cannot be read](errors.md#a-source-cannot-be-read)).

### Symbolic links

Symbolic links inside the directory are not followed. The links themselves are not made documents either. If the location of the directory to ingest is itself a symbolic link, the directory it points to is ingested.

To ingest the location that a link points to, make that location a separate collection.

### `.gitignore`

When the directory to ingest is inside a git repository, files excluded by `.gitignore` rules are not made documents. Being inside a repository means that the directory or a directory above it contains `.git` (a directory or a file) or a `.jj` directory.

- The rules of `.gitignore` files in directories above also apply to the files inside. They do not apply to the directory to ingest itself.
- `.git/info/exclude`, the user's global exclude settings, and `.ignore` are not read.

If you want mdya to ingest files that `.gitignore` excludes, move those rules from `.gitignore` to `.git/info/exclude`. git excludes the files with either rule, and mdya does not read `.git/info/exclude`.

### What is not made a document

- Anything that is not a regular file (devices, named pipes, and so on).
- Files whose relative path cannot be read as UTF-8 (on Windows, paths that contain an unpaired surrogate and cannot be read as Unicode characters) are counted as unreadable documents ([Errors and how to fix them: Some documents cannot be read](errors.md#some-documents-cannot-be-read)).

## Archive

The entries inside an archive file become documents. mdya reads the archive without extracting it to disk.

### Supported archives

| Format | Extension |
|---|---|
| zip | `.zip` |
| tar | `.tar` |
| gzip-compressed tar | `.tar.gz`, `.tgz` |

The format is determined from the extension of the archive's location, ignoring case. Other formats, such as tar compressed with bzip2, xz, or zstd, 7z, and rar, are not supported.

If the archive's location is itself a symbolic link, the archive it points to is ingested.

### What is made a document

Entries in the archive whose extension is `md`, `markdown`, or `pdf` become documents (the same rule as for directories). The following are not made documents.

- Entries that are not regular files (directories, symbolic links, hard links, devices, and so on). Link entries do not become documents, but if a link points to an entry in the archive that becomes a document, that content can be found by search as the document with the target's name.
- Entries for the extra information that macOS adds: entries whose top level is `__MACOSX/`, and entries whose last path component name starts with `._`.
- Archives inside the archive (a `.zip` inside it, and so on). They are not opened and not made documents. If documents inside do not appear in search results, check whether the archive contains another archive.

`.gitignore` files inside the archive are not used.

Sparse files in a tar (files stored with their all-zero parts left out) are made documents under the same conditions as regular files. They are read as the original file, with the left-out parts filled with zeros.

For a tar, nothing after the two all-zero blocks that mark the end of the tar is read. So in a file made by joining several tars, the entries from the second tar onward do not become documents.

### Key

The name in the archive is used as it is, including the top-level directory.

- `\` in a name is treated as a separator and replaced with `/`.
- Path components that are only `.` and empty path components are removed (`./docs//a.md` becomes `docs/a.md`).
- `..` components and control characters are put into the key as they are, because mdya does not write the entries to disk.
- If several entries have the same key, the entry that comes later in the archive is used. For zip, which entry comes later is decided by the order in the list of entries at the end of the zip (the central directory).
- Names are read as UTF-8. This also applies to zip entries without the flag that marks the name as UTF-8 (bit 11 of the general purpose bit flag). However, if an entry without that flag has an extra record that holds the name in UTF-8 (the Info-ZIP Unicode Path Extra Field), the version of that record is 1, and the CRC32 stored in the record matches that entry's name in the list at the end of the zip, the name in that record is used.

### Document versions

Versions are distinguished by the last modified time of the archive itself and the document's body. Every document inside uses the archive's last modified time.

### Entries that become unreadable documents

Among the entries that become documents, the following are counted as unreadable documents ([Errors and how to fix them: Some documents cannot be read](errors.md#some-documents-cannot-be-read)). If there is a document version ingested earlier, it is kept as it is.

- Entries whose size after extraction exceeds 256 MiB (1 MiB is 1,048,576 bytes). Reading stops at the point where the size exceeds it.
- In a zip, entries compressed with a method other than no compression and deflate, and entries that are encrypted.
- Entries whose name cannot be read as UTF-8. They have no key, so there is no earlier document version either.

### When the whole archive cannot be read

The following archives are treated as not readable as a whole. The documents of that collection stay as they were before the update ([Errors and how to fix them: A source cannot be read](errors.md#a-source-cannot-be-read)).

Damaged archives:

- Archives that are cut off partway. For example, a file whose download or copy stopped partway.
- zip archives whose list of entries (central directory) or a record in it is damaged.
- zip archives in which the compressed data of several entries overlap. This is because it would let a small zip hold any number of entries that become large when extracted.
- tar archives in which the checksum of an entry header does not match, or in which a record of a pax extended header is malformed.
- gzip-compressed tar archives in which the check value of the extracted data (CRC32) does not match somewhere up to and including the part that contains the end of the tar (the two all-zero blocks). Anything after the gzip part (member) that contains the end of the tar is not read, so it is not checked.

Archives that are not damaged but are in a form that cannot be read:

- tar archives (including gzip-compressed tar) that do not end with the two all-zero blocks that mark the end of the tar. This is because such an archive cannot be told apart from one cut off exactly at a boundary between entries, and treating it as read would remove the documents past the cut. A tar made in a way that does not write this end cannot be read, even if the entries in it are correct.
- zip archives split into multiple files.
- zip archives that have other data (such as a self-extracting program) attached at the start of the file, where the positions inside the zip have not been adjusted.

Other cases:

- The archive file does not exist, or there is no permission to read it.
- The size of the list of contents exceeds its limit. This is when, counting the number of bytes of the name plus 128 bytes for each entry that becomes a document (including unreadable entries), the total exceeds 256 MiB. Entries that have the same key are also counted one by one. Entries that are not made documents are not counted. However, for an entry whose name is still being read, the bytes of the name read so far are also included in the total and compared, until it is decided whether the entry becomes a document. So, for an entry with a very long name, the limit may be exceeded before it is decided whether the entry becomes a document, even if it is an entry that is not made a document.
- The last modified time or size of the archive changed while the list of contents was being read. This is so that documents are not removed based on a broken list from an archive in the middle of being rewritten.

### Size and memory

- The limit on the size of one entry after extraction (256 MiB) and the limit on the size of the list of contents (256 MiB) cannot be changed in the settings.
- There is no limit on the total size of the documents in an archive. Because of compression and sparse files, many documents of up to 256 MiB each may come out of a small archive. When you ingest an archive whose creator you do not know, expect ingestion to take correspondingly longer and the index to grow correspondingly larger.
- Documents in an archive are read one at a time during ingestion. They are not read ahead and held. The number of documents processed at the same time is determined by the parallelism, so the memory that mdya uses grows as you raise the parallelism ([Settings: Memory and parallelism](configuration.md#memory-and-parallelism)).

## Raw

Document content that you pass with `mdya put` is made a document.

- Source: The file you pass with `mdya put`, or standard input.
- Key and file type: You pass them with `mdya put`.
- Document version: Versions are distinguished by the document's body only. If the document's body is the same as before, it is not ingested again unless `--force` is given.
- Update: Not possible. This is because there is no way to look up the latest information of the source.
- A document leaves the index only when its collection is removed, or when the document is removed with `mdya remove`.

Documents in a raw collection cannot be ingested again with an update. To restore one after it has been removed, you need to pass the same document content with `mdya put` again. Keep a copy of the document content.
