use tiny_editor::coordinates::*;

#[test]
fn test_coordinate_transformations() {
    let viewport = Viewport::new(800.0, 600.0, 2.0); // 2x scale (retina)

    // Doc → Layout → View → Physical
    let doc_pos = DocPos {
        byte_offset: 0,
        line: 5,
        column: 10,
    };

    let layout_pos = viewport.doc_to_layout(doc_pos);
    assert_eq!(
        layout_pos.x,
        LogicalPixels(viewport.margin.x.0 + 10.0 * viewport.metrics.space_width)
    );
    assert_eq!(
        layout_pos.y,
        LogicalPixels(viewport.margin.y.0 + 5.0 * viewport.metrics.line_height)
    );

    let view_pos = viewport.layout_to_view(layout_pos);
    assert_eq!(view_pos.x, layout_pos.x); // No scroll initially
    assert_eq!(view_pos.y, layout_pos.y);

    let physical_pos = viewport.view_to_physical(view_pos);
    assert_eq!(physical_pos.x, PhysicalPixels(view_pos.x.0 * 2.0)); // 2x scale
    assert_eq!(physical_pos.y, PhysicalPixels(view_pos.y.0 * 2.0));
}

#[test]
fn test_scrolling() {
    let mut viewport = Viewport::new(800.0, 600.0, 1.0);
    viewport.scroll = LayoutPos {
        x: LogicalPixels(100.0),
        y: LogicalPixels(200.0),
    };

    let layout_pos = LayoutPos {
        x: LogicalPixels(150.0),
        y: LogicalPixels(250.0),
    };
    let view_pos = viewport.layout_to_view(layout_pos);

    assert_eq!(view_pos.x, LogicalPixels(50.0)); // 150 - 100 scroll
    assert_eq!(view_pos.y, LogicalPixels(50.0)); // 250 - 200 scroll
}

#[test]
fn test_visibility_check() {
    let mut viewport = Viewport::new(800.0, 600.0, 1.0);
    viewport.scroll = LayoutPos {
        x: LogicalPixels(100.0),
        y: LogicalPixels(100.0),
    };

    // Visible rectangle
    let visible_rect = LayoutRect {
        x: LogicalPixels(150.0),
        y: LogicalPixels(150.0),
        width: LogicalPixels(100.0),
        height: LogicalPixels(100.0),
    };
    assert!(viewport.is_visible(visible_rect));

    // Off-screen rectangle
    let offscreen_rect = LayoutRect {
        x: LogicalPixels(0.0),
        y: LogicalPixels(0.0),
        width: LogicalPixels(50.0),
        height: LogicalPixels(50.0),
    };
    assert!(!viewport.is_visible(offscreen_rect));
}

#[test]
fn test_tab_handling() {
    let metrics = TextMetrics::new(14.0);

    // Tab should advance to next tab stop
    assert_eq!(metrics.byte_to_column("hello\tworld", 6), 8); // After tab
    assert_eq!(metrics.byte_to_column("\t\t", 0), 0); // Start
    assert_eq!(metrics.byte_to_column("\t\t", 1), 4); // After first tab
    assert_eq!(metrics.byte_to_column("\t\t", 2), 8); // After second tab
}
