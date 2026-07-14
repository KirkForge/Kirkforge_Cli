// Generate kirkforge.1 man page at build time.
// The real CLI definition is shared in src/cli.rs so the man page cannot
// drift from the runtime parser.

use clap::CommandFactory;

// Include the shared CLI definition verbatim. It lives in the library source
// tree but does not depend on any library internals, so it can be compiled
// both as a normal module and inside this build script.
include!("src/cli.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let man = clap_mangen::Man::new(Cli::command());
    let mut buf = vec![];
    man.render(&mut buf)?;
    std::fs::write(out.join("kirkforge.1"), buf)?;
    println!("cargo:rerun-if-changed=src/cli.rs");
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
