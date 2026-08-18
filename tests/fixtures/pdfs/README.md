# PDF test fixtures

Used by `tests/smoke_ingest.rs` to exercise the PDF ingest path
end-to-end:

| File | Purpose |
|---|---|
| `english.pdf` | ASCII baseline. Verifies pdf-extract → chunker → search basic path. |
| `japanese.pdf` | CJK acceptance. Verifies `pdf-extract`'s `adobe-cmap-parser` handles ToUnicode / CIDFont mapping well enough for Japanese ingest. |

## Regeneration

Both fixtures are committed; a fresh clone runs `cargo test` without
touching this directory. To regenerate (e.g. after editing the source
strings in `xtask/generate-test-pdfs/src/main.rs`):

1. Place `NotoSansCJKjp-Regular.otf` in `xtask/generate-test-pdfs/fonts/`
   (download command in that directory's `README.md`).
2. Run `cargo run -p xtask-generate-test-pdfs` from the workspace root.

## License & provenance

Both PDFs are mdya-authored. The text content is original to this
project (no third-party content embedded). The font subset embedded in
each PDF is **Noto Sans CJK JP** under the
[SIL Open Font License v1.1](https://openfontlicense.org/documents/OFL.txt), which permits
distribution of subsetted derivatives in document files. The mdya
repository's `MIT OR Apache-2.0` dual license applies to the PDF
container and the original text content.
