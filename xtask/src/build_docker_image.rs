use std::{path::Path, process::Command};

use clap::Args;
use color_eyre::Section;
use cross::CommandExt;
use std::fmt::Write;

#[derive(Args, Debug)]
pub struct BuildDockerImage {
    #[clap(long, hide = true, env = "GITHUB_REF_TYPE")]
    ref_type: Option<String>,
    #[clap(long, hide = true, env = "GITHUB_REF_NAME")]
    ref_name: Option<String>,
    /// Specify a tag to use instead of the derived one, eg `local`
    #[clap(long)]
    tag: Option<String>,
    #[clap(long, default_value = cross::docker::CROSS_IMAGE)]
    repository: String,
    /// Newline separated labels
    #[clap(long, env = "LABELS")]
    labels: Option<String>,
    /// Provide verbose diagnostic output.
    #[clap(short, long)]
    verbose: bool,
    #[clap(long)]
    dry_run: bool,
    #[clap(long)]
    force: bool,
    #[clap(short, long)]
    push: bool,
    #[clap(
        long,
        value_parser = clap::builder::PossibleValuesParser::new(["auto", "plain", "tty"]), 
        default_value = "auto"
    )]
    progress: String,
    #[clap(long)]
    no_cache: bool,
    #[clap(long)]
    no_fastfail: bool,
    /// Container engine (such as docker or podman).
    #[clap(long)]
    pub engine: Option<String>,
    #[clap(long)]
    from_ci: bool,
    /// Targets to build for
    #[clap()]
    targets: Vec<String>,
}

pub fn build_docker_image(
    BuildDockerImage {
        ref_type,
        ref_name,
        tag: tag_override,
        repository,
        labels,
        verbose,
        dry_run,
        force,
        push,
        progress,
        no_cache,
        no_fastfail,
        from_ci,
        mut targets,
        ..
    }: BuildDockerImage,
    engine: &Path,
) -> cross::Result<()> {
    let metadata = cross::cargo_metadata_with_args(
        Some(Path::new(env!("CARGO_MANIFEST_DIR"))),
        None,
        verbose,
    )?
    .ok_or_else(|| eyre::eyre!("could not find cross workspace and its current version"))?;
    let version = metadata
        .get_package("cross")
        .expect("cross expected in workspace")
        .version
        .clone();
    if targets.is_empty() {
        if from_ci {
            targets = crate::util::get_matrix()?
                .iter()
                .filter(|m| m.os.starts_with("ubuntu"))
                .map(|m| m.target.clone())
                .collect();
        } else {
            targets = walkdir::WalkDir::new(metadata.workspace_root.join("docker"))
                .max_depth(1)
                .contents_first(true)
                .into_iter()
                .filter_map(|e| e.ok().filter(|f| f.file_type().is_file()))
                .filter_map(|f| {
                    f.file_name()
                        .to_string_lossy()
                        .strip_prefix("Dockerfile.")
                        .map(ToOwned::to_owned)
                })
                .collect();
        }
    }
    let gha = std::env::var("GITHUB_ACTIONS").is_ok();
    let mut results = vec![];
    for target in &targets {
        if gha && targets.len() > 1 {
            println!("::group::Build {target}");
        }
        let mut docker_build = Command::new(engine);
        docker_build.args(&["buildx", "build"]);
        docker_build.current_dir(metadata.workspace_root.join("docker"));

        if push {
            docker_build.arg("--push");
        } else {
            docker_build.arg("--load");
        }

        let dockerfile = format!("Dockerfile.{target}");
        let image_name = format!("{}/{target}", repository);
        let mut tags = vec![];

        match (ref_type.as_deref(), ref_name.as_deref()) {
            (Some(ref_type), Some(ref_name)) if ref_type == "tag" && ref_name.starts_with('v') => {
                let tag_version = ref_name
                    .strip_prefix('v')
                    .expect("tag name should start with v");
                if version != tag_version {
                    eyre::bail!("git tag does not match package version.")
                }
                tags.push(format!("{image_name}:{version}"));
                // Check for unstable releases, tag stable releases as `latest`
                if version.contains('-') {
                    // TODO: Don't tag if version is older than currently released version.
                    tags.push(format!("{image_name}:latest"))
                }
            }
            (Some(ref_type), Some(ref_name)) if ref_type == "branch" => {
                tags.push(format!("{image_name}:{ref_name}"));

                if ["staging", "trying"]
                    .iter()
                    .any(|branch| branch != &ref_name)
                {
                    tags.push(format!("{image_name}:edge"));
                }
            }
            _ => {
                if push && tag_override.is_none() {
                    panic!("Refusing to push without tag or branch. Specify a repository and tag with `--repository <repository> --tag <tag>`")
                }
                tags.push(format!("{image_name}:local"))
            }
        }

        if let Some(ref tag) = tag_override {
            tags = vec![format!("{image_name}:{tag}")];
        }

        docker_build.arg("--pull");
        if no_cache {
            docker_build.arg("--no-cache");
        } else {
            docker_build.args(&[
                "--cache-from",
                &format!("type=registry,ref={image_name}:main"),
            ]);
        }

        if push {
            docker_build.args(&["--cache-to", "type=inline"]);
        }

        for tag in &tags {
            docker_build.args(&["--tag", tag]);
        }

        for label in labels
            .as_deref()
            .unwrap_or("")
            .split('\n')
            .filter(|s| !s.is_empty())
        {
            docker_build.args(&["--label", label]);
        }

        docker_build.args(&["-f", &dockerfile]);

        if gha || progress == "plain" {
            docker_build.args(&["--progress", "plain"]);
        } else {
            docker_build.args(&["--progress", &progress]);
        }

        docker_build.arg(".");

        if !dry_run && (force || !push || gha) {
            let result = docker_build.run(verbose);
            if gha && targets.len() > 1 {
                if let Err(e) = &result {
                    // TODO: Determine what instruction errorred, and place warning on that line with appropriate warning
                    println!("::error file=docker/{dockerfile},title=Build failed::{}", e)
                }
            }
            results.push(
                result
                    .map(|_| target.clone())
                    .map_err(|e| (target.clone(), e)),
            );
            if !no_fastfail && results.last().unwrap().is_err() {
                break;
            }
        } else {
            docker_build.print_verbose(true);
            if !dry_run {
                panic!("refusing to push, use --force to override");
            }
        }
        if gha {
            println!("::set-output name=image::{}", &tags[0]);
            if targets.len() > 1 {
                println!("::endgroup::");
            }
        }
    }
    if gha {
        std::env::set_var("GITHUB_STEP_SUMMARY", job_summary(&results)?);
    }
    if results.iter().any(|r| r.is_err()) {
        results.into_iter().filter_map(Result::err).fold(
            Err(eyre::eyre!("encountered error(s)")),
            |report: Result<(), color_eyre::Report>, e| report.error(e.1),
        )?;
    }
    Ok(())
}

pub fn job_summary(
    results: &[Result<String, (String, cross::errors::CommandError)>],
) -> cross::Result<String> {
    let mut summary = "# SUMMARY\n\n".to_string();
    let success: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    let errors: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();

    if !success.is_empty() {
        summary.push_str("## Success\n\n| Target |\n| ------ |\n");
    }

    for target in success {
        writeln!(summary, "| {target} |")?;
    }

    if !errors.is_empty() {
        // TODO: Tee error output and show in summary
        summary.push_str("\n## Errors\n\n| Target |\n| ------ |\n");
    }

    for (target, _) in errors {
        writeln!(summary, "| {target} |")?;
    }
    Ok(summary)
}
