use kirkforge_video::compose::{build_filter_graph, LineSeries, PieSlice, Scene};

fn main() {
    let s = vec![Scene::LineChart {
        title: "Test".into(),
        x_labels: vec!["a".into(), "b".into()],
        series: vec![LineSeries {
            label: "S".into(),
            values: vec![0.1, 0.9],
            color: None,
        }],
        duration_s: 3.0,
        shot: None,
    }];
    let plan = build_filter_graph(&s, 1920, 1080, 30);
    println!("LINE: {}", plan.filter_complex);

    let s2 = vec![Scene::PieChart {
        title: "Pie".into(),
        slices: vec![PieSlice {
            label: "A".into(),
            percent: 50.0,
            color: None,
        }],
        duration_s: 3.0,
        shot: None,
    }];
    let plan2 = build_filter_graph(&s2, 1920, 1080, 30);
    println!("\nPIE: {}", plan2.filter_complex);
}
