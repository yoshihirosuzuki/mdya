#!/usr/bin/env python3
"""Regenerate the archive fixtures with the tools that write each form.

Each fixture is written by a real archiver, so the tests check that mdya
reads what those archivers produce rather than what we think they produce.

Runs on macOS only (resource forks, extended attributes, ditto, F_PUNCHHOLE
on APFS). Needs GNU tar (`gtar`), bsdtar, git, xattr, ditto, zip, gzip,
sw_vers, and this Python's tarfile and zipfile.

    python3 tests/fixtures/archives/generate.py
"""

import gzip
import io
import os
import shutil
import struct
import subprocess
import sys
import tarfile
import warnings
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Under the repository's ignored tmp/, so a failed run leaves nothing to commit.
WORK = HERE.parents[2] / "tmp" / "archive-fixtures"

# Only PATH comes from the caller, so no personal git or tar settings (git's
# global ignore files, TAR_OPTIONS, TAR_WRITER_OPTIONS, ...) reach the
# archives. HOME is an empty directory under WORK.
CLEAN_ENV = {
    "PATH": os.environ["PATH"],
    "HOME": str(WORK / "home"),
    "LC_ALL": "en_US.UTF-8",
    "TZ": "UTC",
}

# 2026-01-01T00:00:00Z, so regenerating does not churn the timestamps.
MTIME = 1767225600

LONG_DIR = "deep/" + "/".join(f"segment-with-a-long-name-{i:04}" for i in range(1, 6))
LONG_PATH = f"{LONG_DIR}/long.md"
JAPANESE_PATH = "日本語.md"

# Data at every 64 KiB with holes between: enough fragments to need several
# extension blocks in the old GNU sparse format (4 in the header, 21 each)
# and a GNU 1.0 map spanning more than one block.
SPARSE_PARTS = 100
SPARSE_STRIDE = 64 * 1024
SPARSE_SIZE = (SPARSE_PARTS + 1) * SPARSE_STRIDE

# fcntl(2) on macOS (sys/fcntl.h): F_PUNCHHOLE takes a struct fpunchhole
# {fp_flags, reserved, fp_offset, fp_length}, and the hole must be aligned to
# the file system block, 4 KiB on APFS.
F_PUNCHHOLE = 99
HOLE_BLOCK = 4096


def run(*args, cwd=None, env=CLEAN_ENV):
    subprocess.run(args, cwd=cwd, env=env, check=True)


def output_of(*args, cwd=None, env=CLEAN_ENV):
    return subprocess.run(
        args, cwd=cwd, env=env, check=True, stdout=subprocess.PIPE
    ).stdout


def expect(condition, message):
    if not condition:
        sys.exit(f"generate.py: {message}")


def write_gzip(name, data):
    (HERE / name).write_bytes(gzip.compress(data, compresslevel=9, mtime=0))


def write_text(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    os.utime(path, (MTIME, MTIME))


def read_tar(name):
    return tarfile.open(HERE / name, "r:gz")


def make_tree(root):
    write_text(root / "notes/short.md", "# Short\n\nA plain document.\n")
    write_text(root / LONG_PATH, "# Long\n\nA document with a path over 100 bytes.\n")
    write_text(root / JAPANESE_PATH, "# 日本語\n\n名前が UTF-8 の文書です。\n")
    os.link(root / "notes/short.md", root / "hard.md")
    os.symlink("notes/short.md", root / "link-to-short.md")
    os.symlink(LONG_PATH, root / "long-link.md")
    # A FIFO named like a document, which must not be read as one.
    os.mkfifo(root / "pipe.md")
    os.utime(root / "pipe.md", (MTIME, MTIME))


def write_sparse(path, size, parts):
    """Write `parts` (offset, data), each inside one 4 KiB block, as a file
    of `size` bytes with holes everywhere else."""
    import fcntl  # Not on Windows, where main() stops first.

    # APFS fills a gap left by seeking past the end with zeros, so write the
    # zeros and punch the holes afterwards.
    with open(path, "wb") as f:
        content = bytearray(size)
        for offset, data in parts:
            content[offset : offset + len(data)] = data
        f.write(content)
        f.flush()
        kept = {offset // HOLE_BLOCK for offset, _ in parts}
        for block in range(-(-size // HOLE_BLOCK)):
            if block not in kept:
                start = block * HOLE_BLOCK
                length = min(HOLE_BLOCK, size - start)
                fcntl.fcntl(f.fileno(), F_PUNCHHOLE, struct.pack("IIqq", 0, 0, start, length))


def make_sparse(root):
    root.mkdir(parents=True)
    parts = [(i * SPARSE_STRIDE, f"# Part {i}\n".encode()) for i in range(SPARSE_PARTS)]
    write_sparse(root / "sparse.md", SPARSE_SIZE, parts)
    write_sparse(root / "few.md", SPARSE_STRIDE + 7, [(0, b"# Head\n"), (SPARSE_STRIDE, b"# Tail\n")])
    write_sparse(root / "all-hole.md", SPARSE_STRIDE, [])
    # A plain document after the sparse ones, to check where the next entry
    # starts once the sparse maps and data are stepped over.
    write_text(root / "zz-after.md", "# After\n\nComes after the sparse files.\n")
    for name in ("sparse.md", "few.md", "all-hole.md"):
        os.utime(root / name, (MTIME, MTIME))
        st = os.stat(root / name)
        expect(st.st_blocks * 512 < st.st_size, f"{name} has no holes on this file system")


GTAR_COMMON = (
    f"--mtime=@{MTIME}",
    "--owner=0",
    "--group=0",
    "--numeric-owner",
    "--sort=name",
)
TREE_MEMBERS = ("deep", "hard.md", "link-to-short.md", "long-link.md", "notes", "pipe.md", JAPANESE_PATH)
SPARSE_MEMBERS = ("all-hole.md", "few.md", "sparse.md", "zz-after.md")


def gtar(name, src, members, *options):
    data = output_of("gtar", "-c", "-f", "-", *GTAR_COMMON, *options, "-C", str(src), *members)
    write_gzip(name, data)


def generate_gnu_tar(tree, sparse):
    gtar("gtar-gnu.tar.gz", tree, TREE_MEMBERS, "--format=gnu")
    gtar("gtar-posix.tar.gz", tree, TREE_MEMBERS, "--format=posix")
    gtar(
        "gtar-posix-global.tar.gz",
        tree,
        ("notes",),
        "--format=posix",
        "--pax-option=comment=fixture",
        # The default name holds the local temporary directory.
        "--pax-option=globexthdr.name=pax_global_header",
    )
    # ustar has no long link names, and v7 has neither long names nor a
    # prefix field.
    gtar("gtar-ustar.tar.gz", tree, ("deep", "hard.md", "notes", "pipe.md", JAPANESE_PATH), "--format=ustar")
    gtar("gtar-v7.tar.gz", tree, ("hard.md", "link-to-short.md", "notes"), "--format=v7")

    sparse_options = ("--sparse", "--hole-detection=raw")
    gtar("gtar-sparse-old.tar.gz", sparse, SPARSE_MEMBERS, "--format=gnu", *sparse_options)
    for version in ("0.0", "0.1", "1.0"):
        gtar(
            f"gtar-sparse-{version}.tar.gz",
            sparse,
            SPARSE_MEMBERS,
            "--format=posix",
            f"--sparse-version={version}",
            *sparse_options,
        )

    with read_tar("gtar-sparse-old.tar.gz") as tf:
        member = tf.getmember("sparse.md")
        expect(member.type == tarfile.GNUTYPE_SPARSE, "old sparse.md is not type S")
        expect(len(member.sparse) > 4 + 21 * 2, "old sparse.md uses fewer than three extension blocks")
    with read_tar("gtar-sparse-1.0.tar.gz") as tf:
        # tarfile names the member after GNU.sparse.name.
        fragments = tf.getmember("sparse.md").sparse
        map_text = f"{len(fragments)}\n" + "".join(f"{o}\n{n}\n" for o, n in fragments)
        expect(len(map_text) > 512, "the 1.0 map of sparse.md fits in one block")
    expected_keys = {"0.0": "GNU.sparse.numblocks", "0.1": "GNU.sparse.map", "1.0": "GNU.sparse.major"}
    for version, key in expected_keys.items():
        with read_tar(f"gtar-sparse-{version}.tar.gz") as tf:
            headers = [m.pax_headers for m in tf.getmembers()]
            expect(any(key in h for h in headers), f"sparse {version} has no {key}")
    with read_tar("gtar-posix-global.tar.gz") as tf:
        expect(tf.pax_headers.get("comment") == "fixture", "no global comment")


def tar_info(name, data=b"", **fields):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mtime = MTIME
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    for key, value in fields.items():
        setattr(info, key, value)
    return info


def python_tar(name, format, members, pax_headers=None):
    buf = io.BytesIO()
    with tarfile.open(
        fileobj=buf,
        mode="w",
        format=format,
        pax_headers=pax_headers,
        encoding="utf-8",
        errors="surrogateescape",
    ) as tf:
        for info, data in members:
            tf.addfile(info, io.BytesIO(data))
    write_gzip(name, buf.getvalue())


def document(path, text):
    data = text.encode()
    return tar_info(path, data), data


def generate_python():
    short = document("notes/short.md", "# Short\n\nA plain document.\n")
    long = document(LONG_PATH, "# Long\n\nA document with a path over 100 bytes.\n")
    japanese = document(JAPANESE_PATH, "# 日本語\n\n名前が UTF-8 の文書です。\n")
    long_link = (tar_info("long-link.md", type=tarfile.SYMTYPE, linkname=LONG_PATH), b"")

    python_tar("python-gnu.tar.gz", tarfile.GNU_FORMAT, [short, long, long_link])
    python_tar("python-ustar.tar.gz", tarfile.USTAR_FORMAT, [short, long])

    # A name that is not UTF-8 makes tarfile write hdrcharset=BINARY.
    latin1 = document(os.fsdecode(b"latin1-\xe9.md"), "# Latin-1 name\n")
    sized = document("sized.md", "# Sized\n\nCarries a pax size record.\n")
    sized[0].pax_headers = {"size": str(sized[0].size)}
    python_tar(
        "python-pax.tar.gz",
        tarfile.PAX_FORMAT,
        [short, long, japanese, long_link, latin1, sized],
        pax_headers={"comment": "fixture"},
    )

    # tarfile writes whatever global records the caller passes, path included.
    python_tar(
        "python-pax-global-path.tar.gz",
        tarfile.PAX_FORMAT,
        [short],
        pax_headers={"path": "global.md"},
    )

    # Only the headers of an 8 GiB member: the tests supply the zeros, so the
    # fixture stays small. Past 8 GiB - 1 the size no longer fits in octal:
    # the pax form writes 0 in the header and the real size in a pax record,
    # the GNU form writes it in base-256.
    big = tar_info("big.bin", size=8 << 30)
    (HERE / "python-8gib-pax.head").write_bytes(big.tobuf(tarfile.PAX_FORMAT, "utf-8", "surrogateescape"))
    (HERE / "python-8gib-gnu.head").write_bytes(big.tobuf(tarfile.GNU_FORMAT, "utf-8", "surrogateescape"))
    after = io.BytesIO()
    with tarfile.open(fileobj=after, mode="w", format=tarfile.PAX_FORMAT) as tf:
        info, data = document("after.md", "# After\n\nComes after the 8 GiB member.\n")
        tf.addfile(info, io.BytesIO(data))
    # Header, one data block and the end marker, without the record padding.
    (HERE / "python-8gib.tail").write_bytes(after.getvalue()[: 4 * 512])

    with read_tar("python-pax.tar.gz") as tf:
        latin1_member = next(m for m in tf.getmembers() if m.name.startswith("latin1-"))
        expect(latin1_member.pax_headers.get("hdrcharset") == "BINARY", "no hdrcharset=BINARY")
        expect("size" in tf.getmember("sized.md").pax_headers, "no pax size record")
    expect(b"\x80" == (HERE / "python-8gib-gnu.head").read_bytes()[124:125], "GNU size not base-256")


def generate_zip():
    buf = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)  # "Duplicate name"
        with zipfile.ZipFile(buf, "w") as zf:
            for name, text in (("a.md", "# First\n"), ("./a.md", "# Second\n"), ("a.md", "# Third\n")):
                # A ZipInfo keeps its own method and attributes, so set what
                # writestr would give a plain name.
                info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = 0o644 << 16
                zf.writestr(info, text)
    (HERE / "python-duplicate-names.zip").write_bytes(buf.getvalue())


def expect_deflate_or_stored(name):
    """Files must be deflated or stored, the two methods mdya reads; tools
    store a small file whose deflated form would not be smaller."""
    with zipfile.ZipFile(HERE / name) as zf:
        methods = {i.compress_type for i in zf.infolist() if not i.is_dir()}
        expect(zipfile.ZIP_DEFLATED in methods, f"{name} has no deflated file")
        expect(methods <= {zipfile.ZIP_DEFLATED, zipfile.ZIP_STORED}, f"{name} uses another method")


def central_sizes(path):
    """The size field of each file's central directory header, as written
    (zipfile reports the value from the zip64 extra field instead)."""
    with zipfile.ZipFile(path) as zf:
        at = zf.start_dir
    data = path.read_bytes()
    sizes = []
    while data[at : at + 4] == b"PK\x01\x02":
        name_len, extra_len, comment_len = struct.unpack_from("<HHH", data, at + 28)
        if not data[at + 46 : at + 46 + name_len].endswith(b"/"):
            sizes.append(struct.unpack_from("<I", data, at + 24)[0])
        at += 46 + name_len + extra_len + comment_len
    return sizes


def generate_zip_tools():
    src = WORK / "zipsrc"
    write_text(src / "notes/short.md", "# Short\n\nA plain document.\n")
    write_text(src / "notes/repeated.md", "# Repeated\n\n" + "A line that deflates well.\n" * 200)
    write_text(src / JAPANESE_PATH, "# 日本語\n\n名前が UTF-8 の文書です。\n")
    write_text(src / "tagged.md", "# Tagged\n\nCarries an extended attribute.\n")
    run("xattr", "-w", "org.example.fixture", "fixture", str(src / "tagged.md"))
    os.utime(src / "tagged.md", (MTIME, MTIME))

    # ditto -c -k --sequesterRsrc --keepParent is what Finder's Compress
    # does: metadata goes under __MACOSX/.
    ditto = HERE / "ditto.zip"
    ditto.unlink(missing_ok=True)
    run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(src), str(ditto))
    with zipfile.ZipFile(ditto) as zf:
        expect(any(n.startswith("__MACOSX/") for n in zf.namelist()), "ditto wrote no __MACOSX entry")
    expect_deflate_or_stored("ditto.zip")

    # zip adds to an existing archive, so start from none.
    info_zip = HERE / "zip-command.zip"
    info_zip.unlink(missing_ok=True)
    run("zip", "-r", "-q", str(info_zip), "notes", JAPANESE_PATH, "tagged.md", cwd=src)
    expect_deflate_or_stored("zip-command.zip")

    # An entry read from standard input (named `-`) makes zip write zip64
    # end records, while the plain end record keeps ordinary values.
    stdin_zip = HERE / "zip-command-stdin.zip"
    stdin_zip.unlink(missing_ok=True)
    with open(src / "notes/short.md", "rb") as stdin:
        subprocess.run(
            ("zip", "-q", str(stdin_zip), "tagged.md", "-"),
            cwd=src,
            env=CLEAN_ENV,
            check=True,
            stdin=stdin,
        )
    data = stdin_zip.read_bytes()
    expect(b"PK\x06\x06" in data and b"PK\x06\x07" in data, "zip wrote no zip64 end records")
    expect(data[-22 + 10 : -22 + 12] != b"\xff\xff", "zip maxed out the end record's entry count")

    # -fz makes zip use zip64 for small files too: each file's central
    # header holds 0xFFFFFFFF for its size, given in the zip64 extra field.
    zip64_zip = HERE / "zip-command-zip64.zip"
    zip64_zip.unlink(missing_ok=True)
    run("zip", "-fz", "-r", "-q", str(zip64_zip), "notes", cwd=src)
    expect_deflate_or_stored("zip-command-zip64.zip")
    sizes = central_sizes(zip64_zip)
    expect(sizes and all(s == 0xFFFFFFFF for s in sizes), "zip -fz left a size field below its maximum")

    # zip stores `.zip` files by default (its -n list), so a small zip that
    # comes last leaves its end record near the end of the outer zip.
    nested = WORK / "nested"
    write_text(nested / "notes/short.md", "# Short\n\nA plain document.\n")
    write_text(nested / "inner/inner.md", "# Inner\n\nInside the stored zip.\n")
    run("zip", "-r", "-q", str(nested / "zz-bundle.zip"), "inner", cwd=nested)
    nested_zip = HERE / "zip-command-nested.zip"
    nested_zip.unlink(missing_ok=True)
    run("zip", "-r", "-q", str(nested_zip), "notes", "zz-bundle.zip", cwd=nested)
    with zipfile.ZipFile(nested_zip) as zf:
        bundle = zf.getinfo("zz-bundle.zip")
        expect(bundle.compress_type == zipfile.ZIP_STORED, "zip compressed the inner zip")
        expect(zf.infolist()[-1] is bundle, "the inner zip is not the last entry")
    expect(nested_zip.read_bytes().count(b"PK\x05\x06") == 2, "no inner end record")


def generate_gzip_layers(tree):
    """The other fixtures are gzipped here; these keep each tool's own gzip
    output, to check how the compressed stream ends."""
    gtar_out = output_of("gtar", "-c", "-z", "-f", "-", *GTAR_COMMON, "-C", str(tree), "notes")
    (HERE / "gtar-gzip.tar.gz").write_bytes(gtar_out)
    common = ("--uid", "0", "--gid", "0", "--uname", "", "--gname", "")
    bsdtar_out = output_of("bsdtar", "-c", "-z", "-f", "-", *common, "-C", str(tree), "notes")
    (HERE / "bsdtar-gzip.tar.gz").write_bytes(bsdtar_out)
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        info, data = document("notes/short.md", "# Short\n\nA plain document.\n")
        tf.addfile(info, io.BytesIO(data))
    (HERE / "python-gzip.tar.gz").write_bytes(buf.getvalue())
    for name in ("gtar-gzip.tar.gz", "bsdtar-gzip.tar.gz", "python-gzip.tar.gz"):
        expect(gzip.decompress((HERE / name).read_bytes()), f"{name} is empty")


def generate_bsdtar(tree, sparse):
    common = ("--uid", "0", "--gid", "0", "--uname", "", "--gname", "")
    data = output_of("bsdtar", "-c", "-f", "-", *common, "-C", str(tree), *TREE_MEMBERS)
    write_gzip("bsdtar-default.tar.gz", data)

    data = output_of("bsdtar", "-c", "-f", "-", "--format", "pax", *common, "-C", str(sparse), *SPARSE_MEMBERS)
    write_gzip("bsdtar-sparse.tar.gz", data)
    with read_tar("bsdtar-sparse.tar.gz") as tf:
        expect(
            all(m.pax_headers.get("GNU.sparse.major") == "1" for m in tf.getmembers() if m.name != "zz-after.md"),
            "bsdtar did not write GNU sparse 1.0",
        )

    # A resource fork over 16 MiB, stored as an extended attribute in the pax
    # header when bsdtar does not write AppleDouble entries.
    xattr = WORK / "xattr"
    write_text(xattr / "big-xattr.md", "# Big extended attribute\n")
    write_text(xattr / "after.md", "# After\n\nComes after the big pax header.\n")
    with open(xattr / "big-xattr.md/..namedfork/rsrc", "wb") as f:
        f.write(b"\0" * (17 << 20))
    os.utime(xattr / "big-xattr.md", (MTIME, MTIME))
    data = output_of(
        "bsdtar", "-c", "-f", "-", "--format", "pax", "--no-mac-metadata", *common,
        "-C", str(xattr), "big-xattr.md", "after.md",
    )
    write_gzip("bsdtar-large-xattr.tar.gz", data)

    with read_tar("bsdtar-large-xattr.tar.gz") as tf:
        sizes = [len(v) for m in tf.getmembers() for v in m.pax_headers.values()]
        expect(max(sizes, default=0) > 16 << 20, "no pax record over 16 MiB")

    # By default bsdtar adds a ._ AppleDouble entry for a file with extended
    # attributes.
    mac = WORK / "mac"
    write_text(mac / "tagged.md", "# Tagged\n\nCarries an extended attribute.\n")
    run("xattr", "-w", "org.example.fixture", "fixture", str(mac / "tagged.md"))
    os.utime(mac / "tagged.md", (MTIME, MTIME))
    data = output_of("bsdtar", "-c", "-f", "-", *common, "-C", str(mac), "tagged.md")
    write_gzip("bsdtar-mac-metadata.tar.gz", data)

    with read_tar("bsdtar-mac-metadata.tar.gz") as tf:
        expect("._tagged.md" in tf.getnames(), "bsdtar wrote no AppleDouble entry")


def generate_git_archive():
    repo = WORK / "repo"
    write_text(repo / "notes/short.md", "# Short\n\nA plain document.\n")
    write_text(repo / "notes/repeated.md", "# Repeated\n\n" + "A line that deflates well.\n" * 200)
    write_text(repo / LONG_PATH, "# Long\n\nA document with a path over 100 bytes.\n")
    write_text(repo / JAPANESE_PATH, "# 日本語\n\n名前が UTF-8 の文書です。\n")
    os.symlink("notes/short.md", repo / "link-to-short.md")
    env = {
        **CLEAN_ENV,
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_AUTHOR_NAME": "fixture",
        "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
        "GIT_AUTHOR_DATE": f"{MTIME} +0000",
        "GIT_COMMITTER_NAME": "fixture",
        "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        "GIT_COMMITTER_DATE": f"{MTIME} +0000",
    }
    run("git", "init", "-q", "-b", "main", cwd=repo, env=env)
    run("git", "add", "-A", cwd=repo, env=env)
    run("git", "commit", "-q", "-m", "fixture", cwd=repo, env=env)
    write_gzip("git-archive.tar.gz", output_of("git", "archive", "--format=tar", "HEAD", cwd=repo, env=env))
    (HERE / "git-archive.zip").write_bytes(output_of("git", "archive", "--format=zip", "HEAD", cwd=repo, env=env))
    (HERE / "git-archive-gzip.tar.gz").write_bytes(
        output_of("git", "archive", "--format=tar.gz", "HEAD", cwd=repo, env=env)
    )

    with read_tar("git-archive.tar.gz") as tf:
        expect("comment" in tf.pax_headers, "git archive wrote no global comment")
        expect(LONG_PATH in tf.getnames(), "git archive lost the long path")
    expect_deflate_or_stored("git-archive.zip")


def tool_versions():
    lines = [f"python {sys.version.split()[0]}"]
    for tool in (("gtar", "--version"), ("bsdtar", "--version"), ("git", "--version")):
        lines.append(output_of(*tool).decode().splitlines()[0])
    zip_version = output_of("zip", "-v").decode().splitlines()
    lines.append(next(line for line in zip_version if line.startswith("This is Zip")))
    lines.append(f"macOS {output_of('sw_vers', '-productVersion').decode().strip()} (ditto)")
    return lines


def main():
    expect(sys.platform == "darwin", "runs on macOS only")
    for tool in ("gtar", "bsdtar", "git", "xattr", "ditto", "zip", "gzip", "sw_vers"):
        expect(shutil.which(tool, path=CLEAN_ENV["PATH"]), f"{tool} is not on PATH")
    for line in tool_versions():
        print(line)
    shutil.rmtree(WORK, ignore_errors=True)
    try:
        (WORK / "home").mkdir(parents=True)
        tree, sparse = WORK / "tree", WORK / "sparse"
        make_tree(tree)
        make_sparse(sparse)
        generate_gnu_tar(tree, sparse)
        generate_python()
        generate_zip()
        generate_zip_tools()
        generate_gzip_layers(tree)
        generate_bsdtar(tree, sparse)
        generate_git_archive()
    finally:
        shutil.rmtree(WORK, ignore_errors=True)
    for path in sorted(HERE.iterdir()):
        if path.suffix in (".gz", ".zip", ".head", ".tail"):
            print(f"{path.name}: {path.stat().st_size} bytes")


if __name__ == "__main__":
    main()
