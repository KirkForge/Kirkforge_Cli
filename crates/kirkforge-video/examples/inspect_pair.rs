use kirkforge_video::compose::{build_filter_graph, Scene};

fn main() {
    let s = vec![
        Scene::Comparison {
            title: None,
            left_label: "A".into(),
            left_value: "1".into(),
            right_label: "B".into(),
            right_value: "2".into(),
            duration_s: 3.0,
            shot: None,
        },
        Scene::ProgressBar {
            title: Some("T".into()),
            progress: 0.5,
            label: None,
            duration_s: 3.0,
            shot: None,
        },
    ];
    let plan = build_filter_graph(&s, 1920, 1080, 30);
    println!("{}", plan.filter_complex);
}
