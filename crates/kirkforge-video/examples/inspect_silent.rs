use kirkforge_video::compose::{build_filter_graph, Composition, Scene};

fn main() {
    let s = vec![
        Scene::Comparison {
            title: Some("X".into()),
            left_label: "A".into(),
            left_value: "1".into(),
            right_label: "B".into(),
            right_value: "2".into(),
            duration_s: 4.0,
            shot: None,
        },
        Scene::Callout {
            title: "Tip".into(),
            body: "Hello".into(),
            kind: "tip".into(),
            duration_s: 3.0,
            shot: None,
        },
    ];
    let comp = Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        scenes: s,
        audio: None,
    };
    let plan = build_filter_graph(&comp.scenes, comp.width, comp.height, comp.fps);
    println!("{}", plan.filter_complex);
}
