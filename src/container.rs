use crate::watch::DISABLED_LABEL;
use anyhow::Result;
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, LogOutput, LogsOptions,
    RemoveContainerOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::image::{BuildImageOptions, CreateImageOptions};
use bollard::models::{
    BuildInfo, ContainerStateStatusEnum, CreateImageInfo, HostConfig, Mount, MountTypeEnum,
};
use bollard::Docker;
use flate2::read::GzEncoder;
use flate2::Compression;
use futures::TryStreamExt;
use futures_core::Stream;
use log::LevelFilter;
use std::fs::File;
use std::io::Read;
use std::process;
use std::process::Command;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering::Relaxed;
use tempfile::TempDir;
use uuid::Uuid;

pub static PID_LABEL: &str = "com.github.aig787.dockyard.pid";
pub static DOCKYARD_COMMAND_LABEL: &str = "com.github.aig787.dockyard.command";

static COMMAND_VERBOSITY: AtomicU8 = AtomicU8::new(0);

pub fn set_command_verbosity(verbosity: u8) {
    COMMAND_VERBOSITY.store(verbosity, Relaxed);
}

fn get_verbosity_arg() -> String {
    let level = COMMAND_VERBOSITY.load(Relaxed);
    if level > 0 {
        format!("-{}", (0..level).map(|_| 'v').collect::<String>())
    } else {
        "".to_string()
    }
}

pub async fn check_image(
    docker: &Docker,
    image: &str,
) -> Result<Option<Vec<CreateImageInfo>>, bollard::errors::Error> {
    match docker.inspect_image(image).await {
        Ok(_) => Ok(None),
        Err(_) => download_image(docker, image).await.map(|r| Some(r)),
    }
}

async fn download_image(
    docker: &Docker,
    image: &str,
) -> Result<Vec<CreateImageInfo>, bollard::errors::Error> {
    log::info!("Pulling {}", image);
    docker
        .create_image(
            Some(CreateImageOptions {
                from_image: image,
                ..Default::default()
            }),
            None,
            None,
        )
        .try_collect::<Vec<_>>()
        .await
}

pub(crate) async fn run_docker_command(
    docker: &Docker,
    container_name: &str,
    image: &str,
    mounts: Option<Vec<Mount>>,
    cmd: Vec<&str>,
    labels: Option<Vec<(&str, &str)>>,
) -> Result<(i64, Vec<LogOutput>)> {
    check_image(docker, image).await?;
    log::debug!(
        "Running '{}' in container {}",
        cmd.join(" "),
        container_name
    );
    log::trace!(
        "Creating container {} with mounts: {:?}",
        container_name,
        mounts
    );
    docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name,
            }),
            Config {
                cmd: Some(cmd),
                image: Some(&image),
                labels: labels.map(|l| l.into_iter().collect()),
                host_config: Some(HostConfig {
                    mounts,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;

    // Run command and wait for it to finish
    docker
        .start_container(&container_name, None::<StartContainerOptions<String>>)
        .await?;
    docker
        .wait_container(&container_name, None::<WaitContainerOptions<String>>)
        .try_collect::<Vec<_>>()
        .await?;
    let inspection = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await?;
    let logs = match inspection.state.as_ref().and_then(|s| s.status) {
        Some(ContainerStateStatusEnum::DEAD) | Some(ContainerStateStatusEnum::REMOVING) => {
            log::trace!("Not pulling logs from dead or removing container");
            vec![]
        }
        _ => {
            let container_logs = docker
                .logs(
                    &container_name,
                    Some(LogsOptions {
                        follow: true,
                        stdout: true,
                        stderr: true,
                        timestamps: false,
                        tail: "all".to_string(),
                        ..Default::default()
                    }),
                )
                .try_collect::<Vec<_>>()
                .await;
            match container_logs {
                Ok(l) => l,
                Err(e) => {
                    log::warn!(
                        "Error retrieving logs from container {}: {}",
                        &container_name,
                        e
                    );
                    vec![]
                }
            }
        }
    };

    log::trace!("Removing container {}", &container_name);
    docker
        .remove_container(
            &container_name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await?;
    Ok((
        inspection.state.and_then(|s| s.exit_code).unwrap_or(0),
        logs,
    ))
}

/// Run command in dockyard Docker container
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `mounts` - Optional list of mounts to use in container
/// * `cmd` - Command to run in container
///
pub async fn run_dockyard_command(
    docker: &Docker,
    mounts: Option<Vec<Mount>>,
    mut args: Vec<&str>,
) -> Result<(i64, Vec<LogOutput>)> {
    let mut cmd = vec!["dockyard"];
    let verbosity = get_verbosity_arg();
    cmd.append(&mut args);
    if !verbosity.is_empty() {
        cmd.push(&verbosity);
    }

    let image = get_or_build_image(&docker).await?;
    let container_name = format!("dockyard_{}", Uuid::new_v4());
    let pid = process::id().to_string();
    let labels = vec![(PID_LABEL, pid.as_str()), (DISABLED_LABEL, "true")];
    run_docker_command(docker, &container_name, &image, mounts, cmd, Some(labels)).await
}

async fn get_or_build_image(docker: &Docker) -> Result<String> {
    match Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .arg("--show-toplevel")
        .output()
    {
        Ok(output) => {
            let output = output
                .stdout
                .into_iter()
                .map(|i| i as char)
                .collect::<String>();
            let output = output.split('\n').collect::<Vec<_>>();
            let rev = output[0];
            let git_root = output[1];
            let image = format!("dockyard:{}", rev);
            log::debug!("Running in git repo, using version {}", &image);
            if docker.inspect_image(&image).await.is_err() {
                log::info!("{} not found, building...", &image);
                let context = build_context(git_root)?;

                let output = docker.build_image(
                    BuildImageOptions {
                        dockerfile: "Dockerfile",
                        t: image.as_str(),
                        q: false,
                        ..Default::default()
                    },
                    None,
                    Some(context.into()),
                );
                stream_output(&image, output).await?;
            }
            Ok(image)
        }
        Err(_) => Ok(format!("dockyard:{}", env!("VERGEN_SEMVER"))),
    }
}

async fn stream_output(
    prefix: &str,
    stream: impl Stream<Item = Result<BuildInfo, bollard::errors::Error>>,
) -> Result<(), bollard::errors::Error> {
    let print_lines = |prefix: &str, buffer: &mut String| {
        if let Some(index) = buffer.rfind('\n') {
            buffer
                .drain(0..index)
                .collect::<String>()
                .lines()
                .for_each(|line| {
                    if !line.is_empty() {
                        log::info!("[{}] {}", prefix, line);
                    }
                })
        }
    };

    let mut buffer = String::new();
    stream
        .try_for_each(|info| {
            if let Some(s) = info.stream {
                buffer.push_str(s.as_str());
            }
            print_lines(prefix, &mut buffer);
            futures::future::ok(())
        })
        .await
}

fn build_context(root: &str) -> Result<Vec<u8>> {
    log::info!("Creating build context");
    let working_dir = TempDir::new()?;
    let output = working_dir.path().join("context.tgz");
    let tar_gz = File::create(&output)?;
    let enc = GzEncoder::new(tar_gz, Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("src", format!("{}/src", root))?;
    tar.append_path("Dockerfile")?;
    tar.append_path("Cargo.toml")?;
    tar.append_path("Cargo.lock")?;
    tar.append_path("build.rs")?;
    tar.finish()?;
    let mut tar_gz = File::open(&output)?;
    let mut contents = Vec::new();
    tar_gz.read_to_end(&mut contents)?;
    Ok(contents)
}

/// Print output logs, returning failure on non-zero exit code
///
/// # Arguments
///
/// * `exit_code` - Exit code from container
/// * `prefix` - Log output prefix
/// * `logs` - Logs from container
///
pub(crate) fn handle_container_output(
    exit_code: i64,
    prefix: &str,
    logs: &[LogOutput],
) -> Result<()> {
    match exit_code {
        0 => {
            print_logs(prefix, logs, LevelFilter::Debug);
            Ok(())
        }
        _ => {
            print_logs(prefix, logs, LevelFilter::Error);
            Err(anyhow!("Docker returned non-zero exit code: {}", exit_code))
        }
    }
}

pub(crate) fn print_logs(prefix: &str, logs: &[LogOutput], level: LevelFilter) {
    for line in logs {
        let line_string = format!("[{}] {}", prefix, line.to_string().trim());
        match level {
            LevelFilter::Info => log::info!("{}", line_string),
            LevelFilter::Error => log::error!("{}", line_string),
            LevelFilter::Debug => log::debug!("{}", line_string),
            _ => log::trace!("{}", line_string),
        }
    }
}

/// Return Mount representing backup directory
pub fn get_backup_directory_mount(directory: String) -> Mount {
    Mount {
        source: Some(directory),
        target: Some("/backup".to_string()),
        typ: Some(MountTypeEnum::BIND),
        ..Default::default()
    }
}

/// Return Mount representing backup volume
pub fn get_backup_volume_mount(volume: String) -> Mount {
    Mount {
        source: Some(volume),
        target: Some("/backup".to_string()),
        typ: Some(MountTypeEnum::VOLUME),
        ..Default::default()
    }
}

pub fn get_bind_mount(directory: String) -> Mount {
    Mount {
        source: Some(directory),
        target: Some("/volume".to_string()),
        typ: Some(MountTypeEnum::BIND),
        ..Default::default()
    }
}

pub fn get_volume_mount(volume: String) -> Mount {
    Mount {
        source: Some(volume),
        target: Some("/volume".to_string()),
        typ: Some(MountTypeEnum::VOLUME),
        ..Default::default()
    }
}
