# pdfcraft-wiki

Layer L4. The LLM wiki: the user's notes as Markdown pages, one per topic, in a folder they choose, with snippets quoted from their PDFs and links between pages. The AI assistant reads the pages that bear on a question and proposes changes, which the user accepts change by change. Sans-IO and dependency-free: the caller reads the folder and writes back what `apply` returns.

## The page format

Plain Markdown, so another editor (Obsidian, a text editor) can open and change the pages, and PdfCraft reads back what it finds. It understands three things and keeps everything else as it is:

- **Title:** the first `# ` heading, else the file name. YAML front matter is skipped.
- **Snippets:** a block quote whose last line links to the document and page:

  ```markdown
  > The central bank raised its rate to 4% in March.
  > — [rates.pdf, p. 1](pdfcraft://open?sha256=<64 hex>&page=1&path=/Users/me/Papers/rates.pdf)
  ```

  The document is known by its SHA-256, so the snippet finds it after it moves. `page` is 1-based, with no leading zeros. `path` is optional; bytes other than letters, digits and `-_.~/` are percent-encoded. A link that names a document or page in any other spelling isn't a snippet.
- **Links:** `[[Title]]`, `[[Title|shown text]]` and `[[Title#Section]]` (which links to Title).

Code blocks are left out of all three. PdfCraft keeps two files itself. `index.md` lists every page with the start of what it says, and is rewritten after each change, but only an index PdfCraft wrote: one of the user's own, as in an Obsidian vault, is left alone. `log.md` gets a dated entry per batch of changes, and is only ever added to.

## API

```rust
Wiki::read(files: Vec<(String, Vec<u8>)>) -> Wiki   // the folder's .md files; bounded
wiki.find(title) / wiki.page(file) / wiki.backlinks(title) / wiki.index_text(heading, note)
wiki.set(file, text) / wiki.remove(file)            // a batch sees the changes before it
apply(&wiki, &Change, &Headings) -> Result<Applied, ApplyError>
file_name(title, taken) -> String                   // a safe, unique file name for a title
snippets(text) / links(text) / title(text)
Source::link() / Source::parse(title, link)
```

`Change` is one of the following:
- `NewPage`, refused when a page of that title exists.
- `AddSnippet`, which goes at the end of the page's Snippets section and makes the section if needed.
- `AddLink`, which goes at the end of the Related section; a page that already links there is refused.
- `AddText`, which adds a paragraph after what the page says, before its sections.
- `Replace`, where `old` must occur exactly once and the replacement must leave every snippet's quote and source as they were.

Section headings are recognised in several languages. New sections take the caller's `Headings`, which are in the interface's language.

## Bounds

The caps are:
- **Pages per folder:** 5,000.
- **One page:** 1 MiB; a larger page is listed but not read, and never changed.
- **Total read:** 64 MiB.
- **One change:** 64 KiB added.
- **One proposal:** 200 changes.
- **Titles:** 200 bytes. Links in a page are capped at 300 bytes each and 1,000 in all; snippets at 2,000.

`file_name` keeps a file name inside the folder:
- **Characters.** No `/ \ : * ? " < > |`, no control characters, and no words made only of dots (`..`).
- **Hidden and reserved names.** No leading dot, no trailing dot or space, no Windows device name, and never `index.md` or `log.md`.
- **Length and collisions.** At most 120 bytes, cut at a character boundary, and numbered (`Title 2.md`) when the name is taken.

## Tests

Unit tests cover:
- reading a folder (titles, the files PdfCraft keeps, front matter, a page too large);
- snippets written and read back, including by hand, in code, and in lookalike links;
- links, file names and every kind of change with its refusals;
- a batch that builds on a page it made, and the index.

Two property tests check that any page text is read and changed without panicking, and that a snippet survives any quote, title, path and page.
