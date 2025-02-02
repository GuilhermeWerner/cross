use std::{
    collections::BTreeMap,
    path::Path,
    process::{Command, Stdio},
};

use clap::Args;
use cross::CommandExt;

// Store raw text data in the binary so we don't need a data directory
// when extracting all targets, or running our target info script.
const TARGET_INFO_SCRIPT: &str = include_str!("target_info.sh");

#[derive(Args, Debug)]
pub struct TargetInfo {
    /// If not provided, get info for all targets.
    targets: Vec<String>,
    /// Provide verbose diagnostic output.
    #[clap(short, long)]
    verbose: bool,
    /// Image registry.
    #[clap(long, default_value_t = String::from("ghcr.io"))]
    registry: String,
    /// Image repository.
    #[clap(long, default_value_t = String::from("cross-rs"))]
    repository: String,
    /// Image tag.
    #[clap(long, default_value_t = String::from("main"))]
    tag: String,
    /// Container engine (such as docker or podman).
    #[clap(long)]
    pub engine: Option<String>,
}

fn target_has_image(target: &str) -> bool {
    let imageless = ["-msvc", "-darwin", "-apple-ios"];
    !imageless.iter().any(|t| target.ends_with(t))
}

fn format_image(registry: &str, repository: &str, target: &str, tag: &str) -> String {
    let mut output = format!("{target}:{tag}");
    if !repository.is_empty() {
        output = format!("{repository}/{output}");
    }
    if !registry.is_empty() {
        output = format!("{registry}/{output}");
    }

    output
}

fn pull_image(engine: &Path, image: &str, verbose: bool) -> cross::Result<()> {
    let mut command = Command::new(engine);
    command.arg("pull");
    command.arg(image);
    if !verbose {
        // capture output to avoid polluting table
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }
    command.run(verbose).map_err(Into::into)
}

fn image_info(
    engine: &Path,
    target: &str,
    image: &str,
    tag: &str,
    verbose: bool,
    has_test: bool,
) -> cross::Result<()> {
    if !tag.starts_with("local") {
        pull_image(engine, image, verbose)?;
    }

    let mut command = Command::new(engine);
    command.arg("run");
    command.arg("-it");
    command.arg("--rm");
    command.args(&["-e", &format!("TARGET={target}")]);
    if has_test {
        command.args(&["-e", "HAS_TEST=1"]);
    }
    command.arg(image);
    command.args(&["bash", "-c", TARGET_INFO_SCRIPT]);

    if !verbose {
        // capture stderr to avoid polluting table
        command.stderr(Stdio::null());
    }
    command.run(verbose).map_err(Into::into)
}

pub fn target_info(
    TargetInfo {
        mut targets,
        verbose,
        registry,
        repository,
        tag,
        ..
    }: TargetInfo,
    engine: &Path,
) -> cross::Result<()> {
    let matrix = crate::util::get_matrix()?;
    let test_map: BTreeMap<&str, bool> = matrix
        .iter()
        .map(|i| (i.target.as_ref(), i.has_test(&i.target)))
        .collect();

    if targets.is_empty() {
        targets = matrix
            .iter()
            .map(|t| t.target.clone())
            .filter(|t| target_has_image(t))
            .collect();
    }

    for target in targets {
        let target = target.as_ref();
        let image = format_image(&registry, &repository, target, &tag);
        let has_test = test_map
            .get(&target)
            .cloned()
            .ok_or_else(|| eyre::eyre!("invalid target name {}", target))?;
        image_info(engine, target, &image, &tag, verbose, has_test)?;
    }

    Ok(())
}
