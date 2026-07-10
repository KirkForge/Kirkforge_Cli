use kirkforge_video::compose::{build_filter_graph, Scene, TerminalStep};

fn main() {
    let scenes: Vec<Scene> = vec![Scene::TerminalScene {
        title: Some("build log".into()),
        prompt: "$ ".into(),
        accent_color: Some("#3aa0ff".into()),
        steps: vec![
            TerminalStep::Cmd {
                text: "ls".into(),
                type_speed: 0.04,
                hold_s: 0.2,
            },
            TerminalStep::Out {
                text: "a.txt".into(),
                hold_s: 0.4,
            },
        ],
        duration_s: 4.0,
        shot: None,
    }];
    let plan = build_filter_graph(&scenes, 1920, 1080, 30);
    println!("{}", plan.filter_complex);
}
