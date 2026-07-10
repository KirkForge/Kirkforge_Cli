use kirkforge_video::compose::{build_filter_graph, Scene};

fn main() {
    let s = vec![Scene::Callout {
        title: "Tip".into(),
        body: "Hello".into(),
        kind: "tip".into(),
        duration_s: 3.0,
        shot: None,
    }];
    let plan = build_filter_graph(&s, 1920, 1080, 30);
    println!("{}", plan.filter_complex);
}
