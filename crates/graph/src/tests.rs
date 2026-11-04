use super::*;
use crate::proposal::{Change, NodeRef, Proposal};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-a-picture";

fn anchor(page: u32) -> Anchor {
    Anchor {
        sha256: "ab".repeat(32),
        paths: vec!["/docs/rates.pdf".into()],
        title: "rates.pdf".into(),
        page,
        rect: Some([10.0, 20.0, 200.0, 60.0]),
        quote: "Rates rose".into(),
    }
}

fn sample() -> (Graph, NodeId, NodeId, NodeId) {
    let mut g = Graph::new("Rates");
    let a = g.add_node(Content::Text { text: "Rates rose three percent.".into() }, Some(anchor(2)), [0.0, 0.0], Author::User).unwrap();
    let b = g.add_node(Content::Concept { label: "Monetary policy".into() }, None, [300.0, 0.0], Author::User).unwrap();
    let c = g
        .add_node(
            Content::Image { image: ImageBytes(Arc::new(PNG.to_vec())), alt: "Chart of rates".into() },
            Some(anchor(4)),
            [0.0, 300.0],
            Author::User,
        )
        .unwrap();
    let supports = g.kind_named("supports").unwrap().id;
    let example = g.kind_named("is an example of").unwrap().id;
    g.add_edge(a, b, supports, Strength::Strong, None, Author::User).unwrap();
    g.add_edge(c, a, example, Strength::Weak, Some("same period".into()), Author::User).unwrap();
    (g, a, b, c)
}

#[test]
fn connections_are_checked() {
    let (mut g, a, b, c) = sample();
    let supports = g.kind_named("SUPPORTS").unwrap().id;
    let related = g.kind_named("is related to").unwrap().id;
    assert_eq!(g.add_edge(a, b, supports, Strength::Weak, None, Author::User), Err(GraphError::Duplicate));
    // A directed kind can go the other way; an undirected one connects a pair once.
    assert!(g.add_edge(b, a, supports, Strength::Weak, None, Author::User).is_ok());
    g.add_edge(a, b, related, Strength::Medium, None, Author::User).unwrap();
    assert_eq!(g.add_edge(b, a, related, Strength::Medium, None, Author::User), Err(GraphError::Duplicate));
    assert_eq!(g.add_edge(a, a, related, Strength::Medium, None, Author::User), Err(GraphError::SelfLoop));
    assert_eq!(g.add_edge(a, NodeId(999), related, Strength::Medium, None, Author::User), Err(GraphError::NoNode(999)));
    assert_eq!(g.add_edge(a, b, KindId(999), Strength::Medium, None, Author::User), Err(GraphError::NoKind(999)));
    assert_eq!(
        g.add_edge(b, c, related, Strength::Medium, Some("x".repeat(MAX_LABEL_BYTES + 1)), Author::User),
        Err(GraphError::TooLong("the label"))
    );
}

#[test]
fn the_users_own_kinds_join_the_default_ones() {
    let (mut g, a, b, _) = sample();
    let defaults = g.kinds().len();
    for line in [Line::Solid, Line::Dashed, Line::Dotted] {
        assert_eq!(Line::parse(line.name()), Some(line));
    }
    assert_eq!(Line::parse("wavy"), None);
    let colour = g.next_kind_colour();
    let hunts = g.add_kind(" hunts ", true, Line::Dashed, colour).unwrap();
    assert_eq!(g.kind(hunts).map(|k| (k.name.as_str(), k.line)), Some(("hunts", Line::Dashed)));
    assert_eq!(g.add_kind("HUNTS", false, Line::Solid, colour), Ok(hunts), "a name already there is that kind");
    assert_ne!(g.next_kind_colour(), colour, "the next kind gets the next colour");
    assert_eq!(g.add_kind("  ", true, Line::Solid, colour), Err(GraphError::TooLong("the kind's name")));
    g.add_edge(a, b, hunts, Strength::Strong, None, Author::User).unwrap();
    while g.kinds().len() < MAX_KINDS {
        let n = g.kinds().len();
        g.add_kind(&format!("kind {n}"), true, Line::Solid, g.next_kind_colour()).unwrap();
    }
    assert_eq!(g.add_kind("one too many", true, Line::Solid, colour), Err(GraphError::Full("kinds of connection")));
    let (back, warnings) = Graph::from_json(&g.to_json().unwrap()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back.kinds().len(), MAX_KINDS);
    assert!(back.kinds().len() > defaults);
}

#[test]
fn nodes_are_checked_and_removing_one_removes_its_connections() {
    let (mut g, a, _, _) = sample();
    assert_eq!(
        g.add_node(Content::Note { text: "x".repeat(MAX_TEXT_BYTES + 1) }, None, [0.0, 0.0], Author::User),
        Err(GraphError::TooLong("the text"))
    );
    assert_eq!(
        g.add_node(Content::Image { image: ImageBytes(Arc::new(b"GIF89a".to_vec())), alt: String::new() }, None, [0.0, 0.0], Author::User),
        Err(GraphError::NotAnImage)
    );
    let mut bad = anchor(0);
    bad.sha256 = "AB".repeat(32);
    assert!(g.add_node(Content::Note { text: "n".into() }, Some(bad), [0.0, 0.0], Author::User).is_err());
    // Positions that aren't finite are kept at the origin; huge ones are clamped.
    let far = g.add_node(Content::Note { text: "far".into() }, None, [f32::NAN, 1.0e12], Author::User).unwrap();
    assert_eq!(g.node(far).unwrap().position, [0.0, MAX_COORD]);
    assert_eq!(g.edges().len(), 2);
    g.remove_node(a).unwrap();
    assert!(g.edges().is_empty(), "both connections touched it");
    assert_eq!(g.remove_node(a), Err(GraphError::NoNode(a.0)));
}

#[test]
fn neighbourhood_and_search() {
    let (g, a, b, c) = sample();
    assert_eq!(g.neighbourhood(b, 1), [a, b].into_iter().collect());
    assert_eq!(g.neighbourhood(b, 2), [a, b, c].into_iter().collect());
    assert!(g.neighbourhood(NodeId(999), 3).is_empty());
    assert_eq!(g.search("monetary"), vec![b]);
    assert_eq!(g.search("RATES.PDF"), vec![a, c]);
    assert!(g.search("  ").is_empty());
}

#[test]
fn undo_and_redo_restore_whole_maps() {
    let (mut g, a, _, _) = sample();
    let mut history = History::default();
    let before = g.clone();
    history.record("Remove node", g.clone());
    g.remove_node(a).unwrap();
    assert_eq!(history.undo_label(), Some("Remove node"));
    assert_eq!(history.undo(&mut g).as_deref(), Some("Remove node"));
    assert_eq!(g, before);
    assert_eq!(history.redo(&mut g).as_deref(), Some("Remove node"));
    assert!(g.node(a).is_none());
    assert_eq!(history.redo(&mut g), None);
    for i in 0..(MAX_UNDO + 5) {
        history.record(&format!("step {i}"), g.clone());
    }
    let mut undone = 0;
    while history.undo(&mut g).is_some() {
        undone += 1;
    }
    assert_eq!(undone, MAX_UNDO);
}

#[test]
fn saved_maps_round_trip() {
    let (g, ..) = sample();
    let (back, warnings) = Graph::from_json(&g.to_json().unwrap()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back, g);
    // Ids keep counting after the saved ones.
    let mut back = back;
    let n = back.add_node(Content::Note { text: "new".into() }, None, [0.0, 0.0], Author::User).unwrap();
    assert!(g.nodes().iter().all(|old| old.id != n));
}

#[test]
fn damaged_maps_keep_what_is_sound() {
    let (g, ..) = sample();
    let mut v: serde_json::Value = serde_json::from_slice(&g.to_json().unwrap()).unwrap();
    v["nodes"][1]["content"] = serde_json::json!({"type": "nonsense"});
    v["edges"].as_array_mut().unwrap().push(serde_json::json!({"id": 1, "from": 1, "to": 3, "kind": 2}));
    v["edges"].as_array_mut().unwrap().push(serde_json::json!({"id": 77, "from": 1, "to": 1, "kind": 2}));
    v["edges"].as_array_mut().unwrap().push(serde_json::json!({"oops": true}));
    v["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"id": 1, "content": {"type": "note", "text": "dup id"}, "anchor": null, "position": [0, 0]}));
    let (back, warnings) = Graph::from_json(v.to_string().as_bytes()).unwrap();
    assert_eq!(back.nodes().len(), 2, "the damaged node and the duplicate id are dropped");
    // The edge to the dropped node, the self-loop and the junk are dropped; the added edge takes
    // the id the dropped one left free.
    assert_eq!(back.edges().len(), 2);
    assert!(back.edges().iter().all(|e| back.node(e.from).is_some() && back.node(e.to).is_some()));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(matches!(Graph::from_json(b"{}"), Err(GraphError::Unreadable(_))));
    assert!(matches!(Graph::from_json(br#"{"format":"pdfcraft-map","version":9}"#), Err(GraphError::WrongVersion(9))));
    assert!(matches!(Graph::from_json(br#"{"format":"other","version":1}"#), Err(GraphError::Unreadable(_))));
}

#[test]
fn tidy_spreads_nodes_and_pulls_connected_ones_together() {
    let mut g = Graph::new("t");
    let ids: Vec<NodeId> = (0..6).map(|i| g.add_node(Content::Note { text: format!("{i}") }, None, [0.0, 0.0], Author::User).unwrap()).collect();
    let related = g.kind_named("is related to").unwrap().id;
    g.add_edge(ids[0], ids[1], related, Strength::Strong, None, Author::User).unwrap();
    let steps = layout::tidy(&mut g, None, 200);
    assert_eq!(steps, 200);
    let p = |g: &Graph, id: NodeId| g.node(id).unwrap().position;
    let d = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    for (i, a) in ids.iter().enumerate() {
        for b in ids.iter().skip(i + 1) {
            assert!(d(p(&g, *a), p(&g, *b)) > 20.0, "nodes no longer stacked");
        }
    }
    let connected = d(p(&g, ids[0]), p(&g, ids[1]));
    let unconnected = d(p(&g, ids[0]), p(&g, ids[5]));
    assert!(connected < unconnected, "{connected} vs {unconnected}");
    // Only the chosen nodes move.
    let pinned = p(&g, ids[2]);
    let only: HashSet<NodeId> = [ids[3]].into_iter().collect();
    layout::tidy(&mut g, Some(&only), 50);
    assert_eq!(p(&g, ids[2]), pinned);
}

#[test]
fn json_canvas_round_trips_notes_and_connections() {
    let (g, ..) = sample();
    let canvas = export::to_json_canvas(&g);
    assert_eq!(canvas["nodes"].as_array().map(Vec::len), Some(3));
    assert_eq!(canvas["edges"][0]["toEnd"], "arrow");
    assert!(canvas["nodes"][0]["text"].as_str().unwrap().contains("rates.pdf, p. 3"));
    let (back, warnings) = export::from_json_canvas(canvas.to_string().as_bytes(), "back").unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back.nodes().len(), 3);
    assert_eq!(back.edges().len(), 2);
    let kinds: Vec<&str> = back.edges().iter().map(|e| back.kind(e.kind).unwrap().name.as_str()).collect();
    assert_eq!(kinds, ["supports", "is an example of"]);
    assert!(matches!(export::from_json_canvas(b"not json", "x"), Err(GraphError::Unreadable(_))));
}

#[test]
fn graphml_is_escaped_xml() {
    let mut g = Graph::new("A & B <map>");
    let a = g.add_node(Content::Note { text: "x < y & \"z\"\u{1}".into() }, None, [0.0, 0.0], Author::User).unwrap();
    let b = g.add_node(Content::Concept { label: "b".into() }, None, [1.0, 1.0], Author::User).unwrap();
    g.add_edge(a, b, KindId(1), Strength::Weak, Some("l'et".into()), Author::User).unwrap();
    let xml = export::to_graphml(&g);
    assert!(xml.contains("A &amp; B &lt;map&gt;"));
    assert!(xml.contains("x &lt; y &amp; &quot;z&quot;</data>"), "{xml}");
    assert!(xml.contains("l&apos;et"));
    assert!(xml.contains("<data key=\"strength\">weak</data>"));
}

#[test]
fn proposals_apply_only_what_is_accepted() {
    let (mut g, a, b, _) = sample();
    let mut p = Proposal::default();
    p.push(
        Change::AddNode { key: "n1".into(), content: Content::Concept { label: "Inflation".into() }, anchor: None, near: Some(a) },
        "named on page 3",
    );
    p.push(Change::AddNode { key: "n2".into(), content: Content::Concept { label: "Wages".into() }, anchor: None, near: None }, "");
    p.push(
        Change::AddEdge {
            from: NodeRef::New { key: "n1".into() },
            to: NodeRef::Existing { id: a },
            kind: "causes".into(),
            strength: Strength::Strong,
            label: None,
        },
        "rates rise against inflation",
    );
    p.push(
        Change::AddEdge {
            from: NodeRef::New { key: "n2".into() },
            to: NodeRef::Existing { id: b },
            kind: "is related to".into(),
            strength: Strength::Weak,
            label: None,
        },
        "",
    );
    p.push(Change::SetText { id: a, text: "rewritten".into() }, "");
    p.push(
        Change::AddEdge {
            from: NodeRef::Existing { id: a },
            to: NodeRef::Existing { id: b },
            kind: "no such kind".into(),
            strength: Strength::Weak,
            label: None,
        },
        "",
    );
    let applied = p.apply(&mut g, &[0, 2, 3, 4, 5]);
    assert_eq!(applied.nodes.len(), 1);
    assert_eq!(applied.edges.len(), 1);
    let skipped: Vec<usize> = applied.skipped.iter().map(|(i, _)| *i).collect();
    assert_eq!(skipped, vec![3, 4, 5], "{:?}", applied.skipped);
    let inflation = applied.nodes[0];
    assert_eq!(g.node(inflation).unwrap().author, Author::Assistant);
    assert_eq!(g.node(a).unwrap().content.text(), "Rates rose three percent.", "a snippet keeps the document's words");
    assert!(g.search("wages").is_empty());
}

proptest::proptest! {
    #[test]
    fn arbitrary_saved_maps_never_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
        let _ = Graph::from_json(&bytes);
        let _ = export::from_json_canvas(&bytes, "x");
    }

    #[test]
    fn random_edits_keep_the_map_consistent(ops in proptest::collection::vec((0u8..5, 0u64..12, 0u64..12, 0u32..12), 0..80)) {
        let mut g = Graph::new("p");
        for (op, x, y, k) in ops {
            match op {
                0 => { let _ = g.add_node(Content::Note { text: format!("{x}") }, None, [x as f32, y as f32], Author::User); }
                1 => { let _ = g.add_edge(NodeId(x), NodeId(y), KindId(k), Strength::Medium, None, Author::User); }
                2 => { let _ = g.remove_node(NodeId(x)); }
                3 => { let _ = g.remove_edge(EdgeId(x)); }
                _ => { let _ = layout::tidy(&mut g, None, 5); }
            }
            for e in g.edges() {
                proptest::prop_assert!(g.node(e.from).is_some() && g.node(e.to).is_some() && e.from != e.to);
            }
        }
        let (back, warnings) = Graph::from_json(&g.to_json().unwrap()).unwrap();
        proptest::prop_assert!(warnings.is_empty());
        proptest::prop_assert_eq!(back.nodes().len(), g.nodes().len());
        proptest::prop_assert_eq!(back.edges().len(), g.edges().len());
    }
}
