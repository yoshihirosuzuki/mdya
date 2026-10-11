# Archive test fixtures

Used by the tests in `src/ingest/archive/` to check that mdya reads
archives the way real archivers write them. Every fixture is written by
the tool named in its file name, not assembled by hand; forms that no
archiver writes are built inside the tests instead.

| File | Written by | Form |
|---|---|---|
| `gtar-gnu.tar.gz` | GNU tar `--format=gnu` | Long names and long link names (`L` / `K`), a hard link, symbolic links, a FIFO named `pipe.md`, a UTF-8 name |
| `gtar-posix.tar.gz` | GNU tar `--format=posix` | pax `path` / `linkpath`, `atime` / `ctime` records on every entry |
| `gtar-posix-global.tar.gz` | GNU tar `--format=posix --pax-option=comment=...` | A global pax header |
| `gtar-ustar.tar.gz` | GNU tar `--format=ustar` | A long name split into `prefix` and `name` |
| `gtar-v7.tar.gz` | GNU tar `--format=v7` | No magic, a regular file typed NUL |
| `gtar-sparse-old.tar.gz` | GNU tar `--format=gnu --sparse` | Old GNU sparse files (type `S`), with several extension blocks |
| `gtar-sparse-0.0.tar.gz` | GNU tar `--format=posix --sparse --sparse-version=0.0` | GNU sparse 0.0 (`GNU.sparse.offset` / `numbytes` records) |
| `gtar-sparse-0.1.tar.gz` | GNU tar `--format=posix --sparse --sparse-version=0.1` | GNU sparse 0.1 (`GNU.sparse.map`) |
| `gtar-sparse-1.0.tar.gz` | GNU tar `--format=posix --sparse --sparse-version=1.0` | GNU sparse 1.0 (map in the data, over more than one block) |
| `gtar-gzip.tar.gz` | GNU tar `-z` | GNU tar's own gzip output |
| `bsdtar-gzip.tar.gz` | bsdtar `-z` | bsdtar's own gzip output, padded with NUL bytes after the gzip member |
| `git-archive-gzip.tar.gz` | `git archive --format=tar.gz` | git's own gzip output |
| `python-gzip.tar.gz` | Python `tarfile`, mode `w:gz` | Python's own gzip output |
| `bsdtar-default.tar.gz` | bsdtar, default format | ustar with `prefix`, pax where needed, numbers ending in a space |
| `bsdtar-sparse.tar.gz` | bsdtar `--format pax` | GNU sparse 1.0 as libarchive writes it |
| `bsdtar-large-xattr.tar.gz` | bsdtar `--format pax --no-mac-metadata` | A pax header of about 40 MB (a resource fork stored as an extended attribute) |
| `bsdtar-mac-metadata.tar.gz` | bsdtar, default format | The `._tagged.md` AppleDouble entry bsdtar adds for a file with an extended attribute |
| `git-archive.tar.gz` | `git archive --format=tar` | Global `comment` header, `prefix`, 7-digit checksums |
| `git-archive.zip` | `git archive --format=zip` | A UTF-8 name with the zip UTF-8 flag, a symbolic link stored as an entry holding its target path, deflated and stored entries |
| `ditto.zip` | macOS `ditto -c -k --sequesterRsrc --keepParent` (what Finder's Compress does) | `__MACOSX/` entries, the top-level folder, a UTF-8 name without the UTF-8 flag, deflated entries |
| `zip-command.zip` | Info-ZIP `zip -r` (the macOS build) | A UTF-8 name without the UTF-8 flag, deflated and stored entries |
| `zip-command-stdin.zip` | Info-ZIP `zip` with an entry read from standard input | zip64 end records next to an end record that keeps its plain values |
| `zip-command-zip64.zip` | Info-ZIP `zip -fz -r` | zip64 for small files: each file's central header holds 0xFFFFFFFF for its size, given in the zip64 extra field |
| `zip-command-nested.zip` | Info-ZIP `zip -r` over a folder whose last file is a zip | The inner zip stored (zip's default for `.zip`), its end record near the end of the outer one; the tests also cut the file short |
| `python-gnu.tar.gz` | Python `tarfile`, `GNU_FORMAT` | `L` / `K` |
| `python-ustar.tar.gz` | Python `tarfile`, `USTAR_FORMAT` | `prefix` |
| `python-pax.tar.gz` | Python `tarfile`, `PAX_FORMAT` | Global `comment`, `path`, `linkpath`, `hdrcharset=BINARY` with a name that is not UTF-8, a `size` record |
| `python-pax-global-path.tar.gz` | Python `tarfile`, `PAX_FORMAT` | A `path` record in the global header, which `tarfile` writes when the caller passes one |
| `python-8gib-pax.head` | Python `tarfile`, `PAX_FORMAT` | Headers of an 8 GiB member: size 0 in the header, the real size in a `size` record |
| `python-8gib-gnu.head` | Python `tarfile`, `GNU_FORMAT` | Header of an 8 GiB member with a base-256 size |
| `python-8gib.tail` | Python `tarfile` | A member and the end-of-archive marker, to follow the 8 GiB of zeros the tests supply |
| `python-duplicate-names.zip` | Python `zipfile` | `a.md`, `./a.md`, `a.md` with different contents |

The four GNU tar sparse fixtures also pass `--hole-detection=raw`, so their
fragments are single 512-byte blocks and the tests cover block-sized
fragments as well as the 4 KiB ones in `bsdtar-sparse.tar.gz` (the holes as
APFS reports them). Every sparse fixture ends with a plain `zz-after.md`, so
the tests can check where the entry after the sparse files starts.

Apart from the four `*-gzip.tar.gz` files, the `.tar.gz` fixtures hold the
tar each tool wrote, gzipped by the script.

## Regeneration

All fixtures are committed; a fresh clone runs `cargo test` without
touching this directory. To regenerate, run from the repository root:

```sh
python3 tests/fixtures/archives/generate.py
```

It runs on macOS only: the large pax header comes from a resource fork,
the AppleDouble entries from an extended attribute, `ditto.zip` from
`ditto`, and the sparse files get their holes from `fcntl(F_PUNCHHOLE)` on
APFS. It also needs GNU tar as `gtar`, bsdtar, git, xattr, ditto, zip, gzip
and sw_vers, and passes them only `PATH` from your environment. The script
stops if a form that depends on the tool version or the file system is
missing (the sparse forms, the pax records, the base-256 size, the large pax
header, the AppleDouble entries, deflated zip entries, zip's zip64 end
records for standard input, the size fields `zip -fz` leaves at their
maximum, the stored inner zip). If it stops partway, restore
the committed fixtures with `git checkout -- tests/fixtures/archives`. The
committed fixtures were written by Python 3.14.8, GNU tar 1.35, bsdtar
3.5.3 (libarchive 3.7.4), git 2.56.0, Info-ZIP zip 3.0 and the `ditto` of
macOS 26.5.2.

Regenerating does not give identical bytes. Some fields carry the time of
the run (the pax `atime` / `ctime` records, the times of directories,
symbolic links and the global pax header), the placeholder names of GNU
sparse 0.1 / 1.0 members carry the process ID, file modes follow the umask,
and the user and group IDs of whoever runs the script are stored by Info-ZIP
`zip` (its Unix extra field, in `zip-command*.zip`) and by `ditto` (the
older Unix extra field in its local headers, in `ditto.zip`). The forms the tests depend on
stay the same.

## License & provenance

All file contents are mdya-authored test text. The mdya repository's
`MIT OR Apache-2.0` dual license applies.
