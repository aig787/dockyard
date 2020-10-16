use anyhow::Result;
use bollard::Docker;
use bollard::container::{CreateContainerOptions, Config, StartContainerOptions, WaitContainerOptions, LogsOptions, LogOutput, RemoveContainerOptions, InspectContainerOptions};
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use uuid::Uuid;
use futures::TryStreamExt;
use log::LevelFilter;
use std::process;
use futures_core::core_reexport::sync::atomic::AtomicU8;
use futures_core::core_reexport::sync::atomic::Ordering::Relaxed;

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

pub(crate) async fn run_docker_command(docker: &Docker, container_name: &str, image: &str, mounts: Option<Vec<Mount>>, cmd: Vec<&str>, labels: Option<Vec<(&str, &str)>>) -> Result<(i64, Vec<LogOutput>)> {
    log::debug!("Running '{}' in container {}", cmd.join(" "), container_name);
    log::trace!("Creating container {} with mounts: {:?}", container_name, mounts);
    docker.create_container(Some(CreateContainerOptions { name: container_name }), Config {
        cmd: Some(cmd),
        image: Some(&image),
        labels: labels.map(|l| l.into_iter().collect()),
        host_config: Some(HostConfig {
            mounts,
            ..Default::default()
        }),
        ..Default::default()
    }).await?;

    // Run command and wait for it to finish
    docker.start_container(&container_name, None::<StartContainerOptions<String>>).await?;
    docker.wait_container(&container_name, None::<WaitContainerOptions<String>>)
        .try_collect::<Vec<_>>()
        .await?;
    let logs = docker.logs(
        &container_name,
        Some(LogsOptions {
            follow: true,
            stdout: true,
            stderr: true,
            timestamps: false,
            tail: "all".to_string(),
            ..Default::default()
        }),
    ).try_collect::<Vec<_>>().await?;

    // Inspect to get exit code
    let inspection = docker.inspect_container(&container_name, None::<InspectContainerOptions>).await?;
    log::trace!("Removing container {}", &container_name);
    docker.remove_container(&container_name, Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
    })).await?;
    Ok((inspection.state.and_then(|s| s.exit_code).unwrap_or(0), logs))
}

/// Run command in dockyard Docker container
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `mounts` - Optional list of mounts to use in container
/// * `cmd` - Command to run in container
///
pub async fn run_dockyard_command(docker: &Docker, mounts: Option<Vec<Mount>>, mut args: Vec<&str>) -> Result<(i64, Vec<LogOutput>)> {
    let mut cmd = vec!["dockyard"];
    let verbosity = get_verbosity_arg();
    cmd.append(&mut args);
    if !verbosity.is_empty() {
        cmd.push(&verbosity);
    }

    let image = get_image(docker).await?;
    let container_name = format!("dockyard_{}", Uuid::new_v4());
    let pid = process::id().to_string();
    let labels = vec![
        ("com.github.aig787.dockyard.pid", pid.as_str())
    ];
    run_docker_command(docker, &container_name, &image, mounts, cmd, Some(labels)).await
}

/// Print output logs, returning failure on non-zero exit code
///
/// # Arguments
///
/// * `exit_code` - Exit code from container
/// * `prefix` - Log output prefix
/// * `logs` - Logs from container
///
pub(crate) fn handle_container_output(exit_code: i64, prefix: &str, logs: &[LogOutput]) -> Result<()> {
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
            _ => log::trace!("{}", line_string)
        }
    }
}

async fn get_image(docker: &Docker) -> Result<String> {
    let image = format!("dockyard:{}", env!("VERGEN_SHA"));
    match docker.inspect_image(&image).await {
        Ok(_) => Ok(image),
        Err(_) => {
            let version = env!("VERGEN_SEMVER");
            log::warn!("Falling back to {}", version);
            Ok(format!("dockyard:{}", version))
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
