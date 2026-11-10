# PDF test fixtures

Used by the unit tests in `src/format/pdf.rs`.

| File | Purpose |
|---|---|
| `english.pdf` | Text in English. |
| `japanese.pdf` | Text in Japanese, to check that CJK characters come out of the font's ToUnicode map. |

These two files are carried over unchanged from before the components were rewritten, so the text inside them still mentions the PDF library and the test file of that time. The tests look for the title line of each file.

The generator is not in this branch, and its font is not committed. The files can be regenerated from the `xtask/generate-test-pdfs` crate on `main`.

## License and provenance

Both PDFs are mdya-authored. The text content is original to this project. The font subset embedded in each PDF is **Noto Sans CJK JP** under the [SIL Open Font License v1.1](https://openfontlicense.org/documents/OFL.txt), which permits distribution of subsetted derivatives in document files. The repository's `MIT OR Apache-2.0` dual license applies to the PDF container and the original text content.
