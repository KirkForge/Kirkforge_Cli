use kirkforge_video::compose::build_filter_graph;
use kirkforge_video::demos;

fn main() {
    let comp = (demos::list()[3].build)(); // showcase
    let plan = build_filter_graph(&comp.scenes, comp.width, comp.height, comp.fps);
    println!("{}", plan.filter_complex);
}
