use proptest::prelude::*;

use super::*;

const SHA: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

fn source(page: u32) -> Source {
    Source { title: "rates.pdf".into(), sha256: SHA.into(), page, path: Some("/Users/me/Papers/rates & co (2026).pdf".into()) }
}

fn wiki(pages: &[(&str, &str)]) -> Wiki {
    Wiki::read(pages.iter().map(|(f, t)| ((*f).to_owned(), t.as_bytes().to_vec())).collect())
}

#[test]
fn pages_are_read_with_their_titles_and_the_files_pdfcraft_keeps_are_left_out() {
    let w = Wiki::read(vec![
        ("Rates.md".into(), b"---\ntags: [x]\n---\n# Interest rates\n\nWhat banks charge.\n".to_vec()),
        ("no heading.md".into(), b"Just text.".to_vec()),
        ("index.md".into(), b"# Index".to_vec()),
        ("LOG.md".into(), b"# Log".to_vec()),
        (".hidden.md".into(), b"# Hidden".to_vec()),
        ("sub/page.md".into(), b"# Sub".to_vec()),
        ("notes.txt".into(), b"# Text".to_vec()),
        ("Big.md".into(), vec![b'a'; MAX_PAGE_BYTES + 1]),
    ]);
    let titles: Vec<&str> = w.pages.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Big", "no heading", "Interest rates"]);
    assert!(w.pages[0].skipped && w.pages[0].text.is_empty());
    assert_eq!(w.warnings, ["Big.md is too large to read"]);
    assert_eq!(w.find("interest RATES").map(|p| p.file.as_str()), Some("Rates.md"));
    assert_eq!(w.find("rates").map(|p| p.file.as_str()), Some("Rates.md"), "by file name too");
    assert_eq!(w.pages[2].summary(), "What banks charge.");
}

#[test]
fn a_snippet_written_is_read_back_with_its_source() {
    let snippet = Snippet { quote: "The central bank raised its rate\n  to 4% in March.".into(), source: source(0) };
    let block = markdown::snippet_block(&snippet);
    assert!(block.starts_with("> The central bank raised its rate to 4% in March.\n> — [rates.pdf, p. 1](pdfcraft://open?sha256="));
    assert!(block.contains("&path=/Users/me/Papers/rates%20%26%20co%20%282026%29.pdf)"), "{block}");
    let text = format!("# Rates\n\n{block}\nSome note.\n\n```\n> fenced\n> — [x, p. 2](pdfcraft://open?sha256={SHA}&page=2)\n```\n");
    let got = snippets(&text);
    assert_eq!(got.len(), 1, "the fenced one is code");
    assert_eq!(got[0].quote, "The central bank raised its rate to 4% in March.");
    assert_eq!(got[0].source, source(0));
    // Written by hand, with quotation marks and a hyphen.
    let hand = format!("> «Inflation fell.»\n- [rates.pdf, p. 4](pdfcraft://open?sha256={SHA}&page=4)\n");
    let hand = hand.replace("\n-", "\n> -");
    assert_eq!(snippets(&hand)[0].quote, "Inflation fell.");
    assert_eq!(snippets(&hand)[0].source.page, 3);
}

#[test]
fn links_that_dont_name_a_document_and_page_exactly_are_not_snippets() {
    for link in [
        format!("pdfcraft://open?sha256={SHA}&page=0"),
        format!("pdfcraft://open?sha256={SHA}&page=01"),
        format!("pdfcraft://open?sha256={}&page=1", SHA.to_uppercase()),
        format!("pdfcraft://open?sha256={}&page=1", &SHA[1..]),
        "pdfcraft://open?page=1".to_owned(),
        format!("https://example.com/?sha256={SHA}&page=1"),
        format!("pdfcraft://open?sha256={SHA}&page=99999999999"),
    ] {
        assert!(Source::parse("x", &link).is_none(), "{link}");
    }
    let bad_path = Source::parse("x", &format!("pdfcraft://open?sha256={SHA}&page=2&path=%ZZ")).unwrap();
    assert_eq!((bad_path.page, bad_path.path), (1, None), "a broken path is dropped, the rest kept");
}

#[test]
fn links_between_pages_are_read_once_each_outside_code() {
    let text = "See [[Inflation]] and [[Rates|interest rates]], [[Rates#History]].\n`x` [[ ]] [[inflation]]\n```\n[[Code]]\n```\n[[Unclosed";
    assert_eq!(links(text), ["Inflation", "Rates"]);
    let long = format!("[[{}]]", "a".repeat(400));
    assert!(links(&long).is_empty());
}

#[test]
fn file_names_from_titles_stay_in_the_folder_and_dont_collide() {
    let taken = vec!["Rates.md".to_owned(), "rates 2.md".to_owned()];
    assert_eq!(file_name("Rates", &taken), "Rates 3.md");
    assert_eq!(file_name("../../etc/passwd", &[]), "etc passwd.md");
    assert_eq!(file_name("a/b\\c:d*e?f\"g<h>i|j", &[]), "a b c d e f g h ij.md", "a | is left out of titles");
    assert_eq!(file_name("...", &[]), "Untitled.md");
    assert_eq!(file_name("  ", &[]), "Untitled.md");
    assert_eq!(file_name("CON", &[]), "CON page.md");
    assert_eq!(file_name("Index", &[]), "Index page.md", "not the index PdfCraft keeps");
    assert_eq!(file_name(".hidden", &[]), "hidden.md");
    let long = file_name(&"é".repeat(100), &[]);
    assert!(long.len() <= MAX_STEM_BYTES + 3 && long.ends_with(".md"));
    assert_eq!(clean_title("A [[link]] | #tag\n title"), "A link tag title");
}

#[test]
fn changes_land_in_their_sections_and_keep_the_rest_of_the_page() {
    let page = "# Rates\n\nWhat banks charge.\n\n## Snippets\n\n> Old quote.\n> — [rates.pdf, p. 2](pdfcraft://open?sha256=".to_owned()
        + SHA
        + "&page=2)\n\n## Notes\n\nMine, kept.\n";
    let w = wiki(&[("Rates.md", &page)]);
    let h = Headings::default();
    let snippet = Change::AddSnippet {
        page: "rates".into(),
        snippet: Snippet { quote: "Rates rose.".into(), source: source(0) },
        note: Some("The key fact.".into()),
    };
    let a = apply(&w, &snippet, &h).unwrap();
    assert_eq!(a.file, "Rates.md");
    let snips = snippets(&a.after);
    assert_eq!(snips.iter().map(|s| s.quote.as_str()).collect::<Vec<_>>(), ["Old quote.", "Rates rose."]);
    assert!(a.after.find("Rates rose.").unwrap() < a.after.find("## Notes").unwrap(), "inside the section");
    assert!(a.after.contains("The key fact.\n") && a.after.ends_with("## Notes\n\nMine, kept.\n"));
    let mut w2 = w.clone();
    w2.set(&a.file, a.after.clone());
    assert_eq!(apply(&w2, &snippet, &h), Err(ApplyError::AlreadyThere));

    // A link makes the Related section, at the end.
    let link = Change::AddLink { page: "Rates".into(), to: "Inflation".into(), note: Some("what rates fight".into()) };
    let after = apply(&w2, &link, &h).unwrap().after;
    assert!(after.ends_with("## Related\n\n- [[Inflation]] — what rates fight\n"), "{after}");
    w2.set("Rates.md", after);
    let second = apply(&w2, &Change::AddLink { page: "Rates".into(), to: "Banks".into(), note: None }, &h).unwrap().after;
    assert!(second.ends_with("- [[Inflation]] — what rates fight\n- [[Banks]]\n"), "{second}");
    assert_eq!(apply(&w2, &Change::AddLink { page: "Rates".into(), to: "inflation".into(), note: None }, &h), Err(ApplyError::AlreadyThere));
    assert_eq!(apply(&w2, &Change::AddLink { page: "Rates".into(), to: "Rates".into(), note: None }, &h), Err(ApplyError::AlreadyThere));

    // Text goes after what the page says, before its sections.
    let text = apply(&w, &Change::AddText { page: "Rates".into(), text: "They rose in 2026.".into() }, &h).unwrap().after;
    assert!(text.starts_with("# Rates\n\nWhat banks charge.\n\nThey rose in 2026.\n\n## Snippets"), "{text}");
    // An Italian page's sections are recognised.
    let it = wiki(&[("Tassi.md", "# Tassi\n\n## Collegata a\n\n- [[Inflazione]]\n")]);
    let h_it = Headings { snippets: "Snippet".into(), related: "Collegata a".into() };
    let after = apply(&it, &Change::AddLink { page: "Tassi".into(), to: "Banche".into(), note: None }, &h_it).unwrap().after;
    assert_eq!(after, "# Tassi\n\n## Collegata a\n\n- [[Inflazione]]\n- [[Banche]]\n");
}

#[test]
fn replacements_are_exact_and_never_change_a_snippet() {
    let page = format!(
        "# Rates\n\nRates rose. Rates rose.\nThey were high.\n\n> Rates rose to 4%.\n> — [rates.pdf, p. 1](pdfcraft://open?sha256={SHA}&page=1)\n"
    );
    let w = wiki(&[("Rates.md", &page)]);
    let h = Headings::default();
    let r = |old: &str, new: &str| apply(&w, &Change::Replace { page: "Rates".into(), old: old.into(), new: new.into() }, &h);
    assert_eq!(r("They were high.", "They were high in March.").unwrap().after.matches("in March").count(), 1);
    assert_eq!(r("Rates rose. Rates", "x"), Err(ApplyError::NotFound).or(r("Rates rose. Rates", "x")));
    assert_eq!(r("Rates rose.", "x"), Err(ApplyError::Ambiguous));
    assert_eq!(r("nowhere", "x"), Err(ApplyError::NotFound));
    assert_eq!(r("", "x"), Err(ApplyError::Empty));
    assert_eq!(r("to 4%", "to 5%"), Err(ApplyError::SnippetChanged));
    assert_eq!(r("page=1)", "page=2)"), Err(ApplyError::SnippetChanged));
    assert_eq!(apply(&w, &Change::AddText { page: "Nope".into(), text: "x".into() }, &h), Err(ApplyError::NoPage("Nope".into())));
}

#[test]
fn a_new_page_gets_its_own_file_and_a_batch_sees_the_pages_made_before() {
    let mut w = wiki(&[("Rates.md", "# Rates\n")]);
    let h = Headings::default();
    assert_eq!(apply(&w, &Change::NewPage { title: "rates".into(), text: String::new() }, &h), Err(ApplyError::PageExists("rates".into())));
    let made = apply(&w, &Change::NewPage { title: "Inflation: a primer".into(), text: "Prices rising.".into() }, &h).unwrap();
    assert_eq!((made.file.as_str(), made.before.as_deref()), ("Inflation a primer.md", None));
    assert_eq!(made.after, "# Inflation: a primer\n\nPrices rising.\n");
    w.set(&made.file, made.after);
    let link = apply(&w, &Change::AddLink { page: "Inflation: a primer".into(), to: "Rates".into(), note: None }, &h).unwrap();
    assert_eq!(link.file, "Inflation a primer.md");
    assert_eq!(w.backlinks("Inflation: a primer").len(), 0);
    w.set(&link.file, link.after);
    assert_eq!(w.backlinks("rates").iter().map(|p| p.file.as_str()).collect::<Vec<_>>(), ["Inflation a primer.md"]);
    let index = w.index_text("Index", "kept by PdfCraft");
    assert_eq!(index, "# Index\n\n<!-- kept by PdfCraft -->\n\n- [[Inflation: a primer]] — Prices rising.\n- [[Rates]]\n");
    w.remove("Rates.md");
    assert!(w.find("Rates").is_none());
}

#[test]
fn a_page_too_large_to_read_is_never_changed() {
    let w = Wiki::read(vec![("Big.md".into(), vec![b'a'; MAX_PAGE_BYTES + 1])]);
    assert_eq!(apply(&w, &Change::AddText { page: "Big".into(), text: "x".into() }, &Headings::default()), Err(ApplyError::Unread));
    let near = wiki(&[("Near.md", &format!("# Near\n\n{}", "a".repeat(MAX_PAGE_BYTES - 20)))]);
    assert_eq!(apply(&near, &Change::AddText { page: "Near".into(), text: "b".repeat(100) }, &Headings::default()), Err(ApplyError::TooLarge));
}

proptest! {
    #[test]
    fn any_page_text_is_read_and_changed_without_panicking(text in "(?s).{0,400}", title in ".{0,40}", add in ".{0,40}") {
        let w = wiki(&[("P.md", &text)]);
        let p = &w.pages[0];
        let _ = (p.snippets(), p.links(), p.summary(), w.index_text("I", "n"), file_name(&title, &[]));
        let h = Headings::default();
        for change in [
            Change::AddText { page: p.title.clone(), text: add.clone() },
            Change::AddLink { page: p.title.clone(), to: title.clone(), note: Some(add.clone()) },
            Change::Replace { page: p.title.clone(), old: add.clone(), new: title.clone() },
            Change::AddSnippet { page: p.title.clone(), snippet: Snippet { quote: add.clone(), source: source(2) }, note: None },
            Change::NewPage { title: title.clone(), text: add.clone() },
        ] {
            if let Ok(a) = apply(&w, &change, &h) {
                prop_assert!(!a.file.contains(['/', '\\']) && a.file.ends_with(".md"));
                if a.before.is_some() {
                    // What the page held is still there.
                    let kept = snippets(&a.after).len() >= p.snippets().len() || matches!(change, Change::Replace { .. });
                    prop_assert!(kept);
                }
            }
        }
    }

    #[test]
    fn a_snippet_survives_any_quote_title_and_path(quote in "[^\\n>]{1,80}", title in "[^\\n]{0,30}", path in "[^\\x00\\n\\r]{1,40}", page in 0u32..100_000) {
        let quote = quote.split_whitespace().collect::<Vec<_>>().join(" ");
        prop_assume!(!quote.is_empty() && !quote.starts_with(['—', '–', '-', '«', '“', '"']));
        let s = Snippet { quote: quote.clone(), source: Source { title: title.clone(), sha256: SHA.into(), page, path: Some(path.clone()) } };
        let text = format!("# T\n\n{}", markdown::snippet_block(&s));
        let back = snippets(&text);
        prop_assert_eq!(back.len(), 1);
        prop_assert_eq!(&back[0].quote, &quote);
        prop_assert_eq!(back[0].source.page, page);
        prop_assert_eq!(back[0].source.path.as_deref(), Some(path.as_str()));
    }
}
