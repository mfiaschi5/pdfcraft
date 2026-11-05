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
        let placed = layout(&g, rect, &GraphView::default()).nodes;
        assert_eq!(placed.len(), 20);
        assert_fits(&placed, rect);
        for (i, (_, a, ra)) in placed.iter().enumerate() {
            for (_, b, rb) in placed.iter().skip(i + 1) {
                assert!((*b - *a).length() + 0.5 >= ra + rb, "overlap at {a:?} and {b:?} in {rect:?}");
            }
        }
    }
    // A wider panel draws larger circles, farther apart.
    let small = layout(&g, Rect::from_min_size(Pos2::ZERO, vec2(280.0, 224.0)), &GraphView::default()).nodes;
    let large = layout(&g, Rect::from_min_size(Pos2::ZERO, vec2(520.0, 416.0)), &GraphView::default()).nodes;
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
    let placed = layout(&g, Rect::from_min_size(Pos2::ZERO, vec2(300.0, 240.0)), &GraphView::default()).nodes;
    assert!(placed[0].2 > placed[1].2, "the node with two connections is drawn larger");
    let one = map(1, false);
    let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 240.0));
    let placed = layout(&one, rect, &GraphView::default()).nodes;
    assert!((placed[0].1 - rect.center()).length() < 1.0);
    assert!(layout(&Graph::new("Empty"), rect, &GraphView::default()).nodes.is_empty());
    assert!(layout(&one, Rect::NOTHING, &GraphView::default()).nodes.is_empty());
}

#[test]
fn zooming_and_moving_change_where_nodes_are_drawn() {
    // Three nodes well apart: nothing to move apart, so only the view moves them.
    let g = map(3, true);
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 240.0));
    let fitted = layout(&g, rect, &GraphView::default()).nodes;
    let moved = layout(&g, rect, &GraphView { pan: vec2(40.0, -10.0), ..GraphView::default() }).nodes;
    assert!((moved[0].1 - fitted[0].1 - vec2(40.0, -10.0)).length() < 1.0);
    let zoomed = layout(&g, rect, &GraphView { zoom: 2.0, ..GraphView::default() }).nodes;
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

#[test]
fn the_arrow_keys_go_to_the_nearest_node_that_way() {
    let n = |i: u64| NodeId(i);
    let nodes = vec![
        (n(1), pos2(100.0, 100.0), 8.0),
        (n(2), pos2(200.0, 110.0), 8.0),
        (n(3), pos2(300.0, 100.0), 8.0),
        (n(4), pos2(110.0, 220.0), 8.0),
        (n(5), pos2(160.0, 40.0), 8.0),
    ];
    let from = pos2(100.0, 100.0);
    assert_eq!(neighbour(&nodes, from, vec2(1.0, 0.0), Some(n(1))), Some(n(2)), "the nearer of two to the right");
    assert_eq!(neighbour(&nodes, from, vec2(0.0, 1.0), Some(n(1))), Some(n(4)));
    assert_eq!(neighbour(&nodes, from, vec2(0.0, -1.0), Some(n(1))), Some(n(5)), "up and a little to the side");
    assert_eq!(neighbour(&nodes, from, vec2(-1.0, 0.0), Some(n(1))), None, "nothing to the left");
}

#[test]
fn a_preview_crops_the_page_around_its_passage() {
    let page = vec2(600.0, 800.0);
    // One line in the middle: a good part of the page wide at least, with room above and below.
    let line = [Rect::from_min_max(pos2(100.0, 400.0), pos2(260.0, 412.0))];
    let crop = preview_crop(&line, &[], page, 0.8);
    assert!(crop.contains_rect(line[0]), "{crop:?}");
    assert!(crop.width() >= 600.0 * MIN_CROP - 0.1 && crop.height() >= 12.0 + 2.0 * CONTEXT - 0.1);
    assert!(crop.width() < 600.0, "narrower than the page, so the words show large");
    assert!(Rect::from_min_size(Pos2::ZERO, page).contains_rect(crop));
    // A long passage: no taller than asked, starting where it starts.
    let lines: Vec<Rect> = (0..40).map(|i| Rect::from_min_max(pos2(50.0, 100.0 + i as f32 * 15.0), pos2(550.0, 112.0 + i as f32 * 15.0))).collect();
    let crop = preview_crop(&lines, &[], page, 0.5);
    assert!(crop.height() <= crop.width() * 0.5 + 0.1);
    assert!(crop.min.y <= 100.0 && crop.max.y > 100.0, "the start shows: {crop:?}");
    // At the page's edge, the crop stays on the page.
    let edge = [Rect::from_min_max(pos2(560.0, 5.0), pos2(598.0, 15.0))];
    let crop = preview_crop(&edge, &[], page, 0.8);
    assert!(Rect::from_min_size(Pos2::ZERO, page).expand(0.01).contains_rect(crop) && crop.contains_rect(edge[0]), "{crop:?}");
    // Nothing found yet: the whole page.
    assert_eq!(preview_crop(&[], &[], page, 0.8), Rect::from_min_size(Pos2::ZERO, page));
}

#[test]
fn a_preview_shows_whole_lines_of_the_passages_column() {
    let page = vec2(1000.0, 800.0);
    // A left column of lines 50..450 wide, 15 apart, and a right column beside it at 550..950.
    let left: Vec<Rect> = (0..30).map(|i| Rect::from_min_max(pos2(50.0, 100.0 + i as f32 * 15.0), pos2(450.0, 111.0 + i as f32 * 15.0))).collect();
    let right: Vec<Rect> = (0..30).map(|i| Rect::from_min_max(pos2(550.0, 100.0 + i as f32 * 15.0), pos2(950.0, 111.0 + i as f32 * 15.0))).collect();
    let page_lines: Vec<Rect> = left.iter().chain(&right).copied().collect();
    // The passage starts mid-line 10 and ends mid-line 11.
    let passage = [Rect::from_min_max(pos2(260.0, 250.0), pos2(450.0, 261.0)), Rect::from_min_max(pos2(50.0, 265.0), pos2(200.0, 276.0))];
    let crop = preview_crop(&passage, &page_lines, page, 0.8);
    assert!(crop.min.x <= 50.0 && crop.max.x >= 450.0, "its lines whole across: {crop:?}");
    assert!(crop.max.x < 550.0, "not the column beside it: {crop:?}");
    for l in &left {
        let inside = crop.contains_rect(*l);
        let outside = !crop.intersects(*l);
        assert!(inside || outside, "no line cut: {l:?} by {crop:?}");
    }
    assert!(left.iter().filter(|l| crop.contains_rect(**l)).count() >= 4, "the passage's lines and one around them");
    // A long passage ends on a whole line, its start in view.
    let long: Vec<Rect> = left.get(5..25).unwrap().to_vec();
    let crop = preview_crop(&long, &page_lines, page, 0.3);
    assert!(crop.height() <= crop.width() * 0.3 + 0.1, "{crop:?}");
    assert!(crop.contains_rect(long[0]), "its start: {crop:?}");
    for l in &left {
        assert!(crop.contains_rect(*l) || !crop.intersects(*l), "no line cut: {l:?} by {crop:?}");
    }
}

#[test]
fn a_dragged_node_lands_where_the_pointer_left_it() {
    let g = map(3, false);
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 240.0));
    let placed = layout(&g, rect, &GraphView::default());
    let fit = placed.fit;
    let target = pos2(150.0, 60.0);
    let back = fit.to_screen(fit.to_map(target));
    assert!((back - target).length() < 0.01);
}

proptest! {
    #[test]
    fn any_map_is_laid_out_inside_the_panel(xs in proptest::collection::vec((-1.0e9f32..1.0e9, -1.0e9f32..1.0e9), 1..60), w in 120.0f32..900.0) {
        let mut g = Graph::new("P");
        for (x, y) in &xs {
            g.add_node(Content::Note { text: "n".into() }, None, [*x, *y], Author::User).unwrap();
        }
        let rect = Rect::from_min_size(pos2(5.0, 5.0), vec2(w, (w * 0.8).clamp(170.0, 440.0)));
        let placed = layout(&g, rect, &GraphView::default()).nodes;
        prop_assert_eq!(placed.len(), xs.len());
        for (_, c, r) in &placed {
            prop_assert!(c.x.is_finite() && c.y.is_finite() && r.is_finite() && *r > 0.0);
            prop_assert!(rect.expand(0.5).contains(*c), "{:?} outside {:?}", c, rect);
        }
    }
}
