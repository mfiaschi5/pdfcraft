use proptest::prelude::*;

use super::*;

/// A map of `n` concepts laid out as the workspace might have them, some on the same spot.
fn map(n: usize, connected: bool) -> Graph {
    let mut g = Graph::new("Test");
    let kind = g.kinds().first().map(|k| k.id).unwrap();
    let mut ids = Vec::new();
    for i in 0..n {
        let at = [(i % 7) as f32 * 220.0, (i / 7) as f32 * 140.0 + if i % 5 == 0 { 0.0 } else { 3.0 }];
        // Every fourth node on top of the one before, as a paste or an import may leave them.
        let at = if i % 4 == 3 { [((i - 1) % 7) as f32 * 220.0, ((i - 1) / 7) as f32 * 140.0] } else { at };
        ids.push(g.add_node(Content::Concept { label: format!("Concept {i}") }, None, at, Author::User).unwrap());
    }
    if connected {
        for w in ids.windows(2) {
            g.add_edge(w[0], w[1], kind, Strength::Medium, None, Author::User).unwrap();
        }
    }
    g
}

fn assert_fits(placed: &[(NodeId, Pos2, f32)], rect: Rect) {
    for (id, c, r) in placed {
        assert!(rect.contains(*c - Vec2::splat(*r)) && rect.contains(*c + Vec2::splat(*r)), "{id:?} at {c:?} r {r} outside {rect:?}");
    }
}

#[test]
fn the_graph_fits_the_panel_at_any_width_without_circles_overlapping() {
    let g = map(20, true);
    for rect in [Rect::from_min_size(pos2(900.0, 100.0), vec2(280.0, 224.0)), Rect::from_min_size(pos2(700.0, 100.0), vec2(520.0, 416.0))] {
        let placed = layout(&g, rect, &PanelView::default());
        assert_eq!(placed.len(), 20);
        assert_fits(&placed, rect);
        for (i, (_, a, ra)) in placed.iter().enumerate() {
            for (_, b, rb) in placed.iter().skip(i + 1) {
                assert!((*b - *a).length() + 0.5 >= ra + rb, "overlap at {a:?} and {b:?} in {rect:?}");
            }
        }
    }
    // A wider panel draws larger circles, farther apart.
    let small = layout(&g, Rect::from_min_size(Pos2::ZERO, vec2(280.0, 224.0)), &PanelView::default());
    let large = layout(&g, Rect::from_min_size(Pos2::ZERO, vec2(520.0, 416.0)), &PanelView::default());
    assert!(large[0].2 > small[0].2);
    assert!((large[0].1 - large[1].1).length() > (small[0].1 - small[1].1).length());
}

#[test]
fn connected_nodes_are_larger_and_a_single_node_sits_in_the_middle() {
    let mut g = map(3, false);
    let ids: Vec<NodeId> = g.nodes().iter().map(|n| n.id).collect();
    let kind = g.kinds().first().map(|k| k.id).unwrap();
    g.add_edge(ids[0], ids[1], kind, Strength::Strong, None, Author::User).unwrap();
    g.add_edge(ids[0], ids[2], kind, Strength::Weak, None, Author::User).unwrap();
    let placed = layout(&g, Rect::from_min_size(Pos2::ZERO, vec2(300.0, 240.0)), &PanelView::default());
    assert!(placed[0].2 > placed[1].2, "the node with two connections is drawn larger");
    let one = map(1, false);
    let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 240.0));
    let placed = layout(&one, rect, &PanelView::default());
    assert!((placed[0].1 - rect.center()).length() < 1.0);
    assert!(layout(&Graph::new("Empty"), rect, &PanelView::default()).is_empty());
    assert!(layout(&one, Rect::NOTHING, &PanelView::default()).is_empty());
}

#[test]
fn zooming_and_moving_change_where_nodes_are_drawn() {
    // Three nodes well apart: nothing to move apart, so only the view moves them.
    let g = map(3, true);
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 240.0));
    let fitted = layout(&g, rect, &PanelView::default());
    let moved = layout(&g, rect, &PanelView { zoom: 1.0, pan: vec2(40.0, -10.0) });
    assert!((moved[0].1 - fitted[0].1 - vec2(40.0, -10.0)).length() < 1.0);
    let zoomed = layout(&g, rect, &PanelView { zoom: 2.0, pan: Vec2::ZERO });
    let spread = |p: &[(NodeId, Pos2, f32)]| (p[0].1 - p[2].1).length();
    assert!(spread(&zoomed) > spread(&fitted) * 1.5);
}

#[test]
fn labels_are_cut_at_a_word_and_kept_inside_the_graph() {
    assert_eq!(short_label("Inflazione", 22), "Inflazione");
    assert_eq!(short_label("L'effetto arriva con mesi di ritardo", 22), "L'effetto arriva con…");
    assert_eq!(short_label("Unaparolalunghissimasenzaspazi", 10), "Unaparolal…");
    assert_eq!(short_label("Prima riga\nseconda", 22), "Prima riga");
    assert_eq!(short_label("àèìòù àèìòù àèìòù àèìòù", 8), "àèìòù…");
    let bounds = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0));
    let left = inside(Rect::from_min_size(pos2(-30.0, 10.0), vec2(60.0, 12.0)), bounds);
    assert_eq!(left.min.x, 0.0);
    let right = inside(Rect::from_min_size(pos2(180.0, 95.0), vec2(60.0, 12.0)), bounds);
    assert_eq!((right.max.x, right.max.y), (200.0, 100.0));
}

proptest! {
    #[test]
    fn any_map_is_laid_out_inside_the_panel(xs in proptest::collection::vec((-1.0e9f32..1.0e9, -1.0e9f32..1.0e9), 1..60), w in 120.0f32..900.0) {
        let mut g = Graph::new("P");
        for (x, y) in &xs {
            g.add_node(Content::Note { text: "n".into() }, None, [*x, *y], Author::User).unwrap();
        }
        let rect = Rect::from_min_size(pos2(5.0, 5.0), vec2(w, (w * 0.8).clamp(170.0, 440.0)));
        let placed = layout(&g, rect, &PanelView::default());
        prop_assert_eq!(placed.len(), xs.len());
        for (_, c, r) in &placed {
            prop_assert!(c.x.is_finite() && c.y.is_finite() && r.is_finite() && *r > 0.0);
            prop_assert!(rect.expand(0.5).contains(*c), "{:?} outside {:?}", c, rect);
        }
    }
}
