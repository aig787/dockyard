use std::fs::{copy, create_dir_all, File};
use std::path::{Path, PathBuf};

use crate::container::{handle_container_output, run_dockyard_command};
use anyhow::{Context, Result};
use bollard::container::InspectContainerOptions;
use bollard::models::{
    ContainerConfig, ContainerInspectResponse, HostConfig, Mount, MountPoint, MountTypeEnum,
};
use bollard::Docker;
use chrono::Utc;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures::future::*;
use std::collections::HashSet;

/// Backup of volume/directory contents and mount info
#[derive(Serialize, Deserialize, Debug)]
pub struct MountBackup {
    pub(crate) path: PathBuf,
    pub(crate) mount: MountPoint,
}

/// Backup of container configs with links to volume/directory backups
#[derive(Serialize, Deserialize, Debug)]
pub struct ContainerBackup {
    pub(crate) name: String,
    pub(crate) container_config: ContainerConfig,
    pub(crate) host_config: HostConfig,
    pub(crate) mounts: Vec<MountBackup>,
}

/// Back up directory as tarball
///
/// # Arguments
///
/// * `name` - Name of output archive
/// * `input` - Directory to back up
/// * `output` - Output directory of archive
///
pub fn backup_directory(input: &str, output: &str) -> Result<PathBuf> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);
    let name = Utc::now().to_rfc3339();

    let path = if input_path.is_dir() {
        let backup_path = output_path.join(format!("{}.tgz", &name));
        create_directory(backup_path.as_path())?;
        log::info!(
            "Backing up directory {} to {}",
            input_path.display(),
            backup_path.display()
        );
        let archive = File::create(&backup_path)
            .with_context(|| format!("Unable to create file {}", &backup_path.display()))?;
        let enc = GzEncoder::new(archive, Compression::default());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all("", input_path).with_context(|| {
            format!(
                "Failed to create tarball {} from {}",
                &backup_path.display(),
                input
            )
        })?;
        backup_path
    } else {
        let backup_path = output_path.join(&name);
        create_directory(backup_path.as_path())?;
        log::info!(
            "Backing up file {} to {}",
            input_path.display(),
            &backup_path.display()
        );
        copy(input_path, &backup_path)?;
        backup_path
    };
    Ok(path.strip_prefix(output_path)?.to_path_buf())
}

fn create_directory(path: &Path) -> Result<()> {
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().unwrap()
    };
    log::info!("Creating directory {}", directory.display());
    create_dir_all(directory)?;
    Ok(())
}

pub async fn backup_directory_to_mount(
    docker: &Docker,
    input: String,
    output: String,
    mount: Mount,
) -> Result<PathBuf> {
    log::info!(
        "Backing up directory {} to {}/ on {}",
        &input,
        output,
        mount.source.as_ref().unwrap()
    );
    let mounted_input = Path::new("/input");
    let mounted_output = Path::new(mount.target.as_ref().unwrap()).join(&output);
    let log_prefix = format!("backup directory {}", &input);
    let input_mount = Mount {
        source: Some(input),
        target: Some("/input".to_string()),
        typ: Some(MountTypeEnum::BIND),
        ..Default::default()
    };
    let args = vec![
        "backup",
        "directory",
        mounted_input.to_str().unwrap(),
        mounted_output.to_str().unwrap(),
    ];
    let (exit_code, logs) =
        run_dockyard_command(docker, Some(vec![input_mount, mount]), args).await?;
    let output_path = logs
        .last()
        .unwrap()
        .to_string()
        .trim()
        .split_ascii_whitespace()
        .last()
        .unwrap()
        .to_string();
    handle_container_output(exit_code, &log_prefix, &logs)
        .map(|_| Path::new(&output).join(output_path))
}

/// Back up volume
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `volume` - Name of volume to back up
/// * `backup_mount` - Mount of backup destination
///
pub async fn backup_volume(
    docker: &Docker,
    volume: String,
    backup_mount: Mount,
) -> Result<PathBuf> {
    let mounts = vec![
        Mount {
            source: Some(volume.to_string()),
            target: Some("/volume".to_string()),
            typ: Some(MountTypeEnum::VOLUME),
            ..Default::default()
        },
        backup_mount,
    ];
    let output = Path::new("dockyard/volumes").join(&volume);
    log::info!(
        "Backing up volume {} to {} on {}",
        &volume,
        output.display(),
        mounts[0].source.as_ref().unwrap()
    );
    let mounted_output = Path::new("/backup").join(&output);
    let args = vec![
        "backup",
        "directory",
        "/volume",
        mounted_output.to_str().unwrap(),
    ];
    let log_prefix = format!("backup volume {}", &volume);
    match run_dockyard_command(docker, Some(mounts), args).await {
        Ok((exit_code, logs)) => handle_container_output(exit_code, &log_prefix, &logs).map(|_| {
            let archive_name = logs
                .last()
                .unwrap()
                .to_string()
                .trim()
                .split_ascii_whitespace()
                .last()
                .unwrap()
                .to_string();
            output.join(archive_name)
        }),
        Err(e) => Err(e),
    }
}

/// Back up container
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `container_info` - Container inspection result
/// * `mounts` - List of mounts to back up
/// * `output` - Output directory relative to `backup_mount`
/// * `backup_mount` - Mount representing backup destination
///
pub async fn backup_container(
    docker: &Docker,
    container_name: &str,
    backup_mount: Mount,
    exclude_volumes: &HashSet<String>,
) -> Result<PathBuf> {
    let output = Path::new("dockyard/containers").join(container_name);
    log::info!(
        "Backing up container {} to {}",
        container_name,
        output.display()
    );
    let (info, mounts) = get_container_info(docker, container_name, exclude_volumes).await?;
    let mut mount_backup_processes = vec![];
    for mp in mounts {
        if mp.typ.as_ref().unwrap() == "bind" {
            if mp.source.as_ref().unwrap() == "/var/run/docker.sock" {
                log::info!("Ignoring bind /var/run/docker.sock")
            } else {
                let output = format!(
                    "dockyard/binds/{}",
                    mp.source.as_ref().unwrap().replace("/", ":")
                );
                let directory = mp.source.as_ref().unwrap().clone();
                mount_backup_processes.push((
                    mp,
                    Either::Left(backup_directory_to_mount(
                        docker,
                        directory,
                        output,
                        backup_mount.clone(),
                    )),
                ));
            }
        } else {
            let volume_name = mp.name.as_ref().unwrap().clone();
            mount_backup_processes.push((
                mp,
                Either::Right(backup_volume(docker, volume_name, backup_mount.clone())),
            ));
        }
    }
    let mount_backups = validate_process_results(mount_backup_processes).await?;
    let container_backup = ContainerBackup {
        name: container_name.to_string(),
        container_config: info.config.unwrap(),
        host_config: info.host_config.unwrap(),
        mounts: mount_backups,
    };
    write_container_backup(docker, container_backup, output, backup_mount).await
}

/// Include only bind mounts and non-network volumes
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `mount` - Mount to inspect and filter
///
async fn filter_mount(
    docker: &Docker,
    mount: &MountPoint,
    exclude_volumes: &HashSet<String>,
) -> Result<bool> {
    match mount.typ.as_deref() {
        Some("volume") => {
            let volume_name = mount.name.as_ref().unwrap();
            let volume = docker.inspect_volume(volume_name).await?;
            match volume.options.get("type").map(String::as_str) {
                Some("nfs") | Some("nfs4") => {
                    log::info!("Ignoring network volume {}", volume_name);
                    Ok(false)
                }
                _ => {
                    if exclude_volumes.contains(volume_name) {
                        log::info!("Ignoring excluded volume {}", volume_name);
                        Ok(false)
                    } else {
                        log::info!("Including volume {}", volume_name);
                        Ok(true)
                    }
                }
            }
        }
        Some("bind") => Ok(true),
        Some(t) => {
            log::info!("Ignoring mount with type {}", t);
            Ok(false)
        }
        _ => {
            log::info!("Ignoring mount {:?}", &mount);
            Ok(false)
        }
    }
}

/// Return Container Inspection and mounts for container
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `container_name` - Name of container to inspect
/// * `volumes` - Optional list of volumes to retrieve
///
async fn get_container_info(
    docker: &Docker,
    container_name: &str,
    exclude_volumes: &HashSet<String>,
) -> Result<(ContainerInspectResponse, Vec<MountPoint>)> {
    let container_info = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await?;
    let mut filtered_mounts = vec![];
    for mp in container_info.mounts.as_ref().unwrap() {
        if filter_mount(docker, mp, exclude_volumes).await? {
            filtered_mounts.push(mp.clone())
        }
    }
    Ok((container_info, filtered_mounts))
}

/// Await volume backups and return a MountBackup for each
///
/// # Arguments
///
/// * `backup_results` - List of volume backup results
///
async fn validate_process_results(
    backup_results: Vec<(
        MountPoint,
        Either<impl Future<Output = Result<PathBuf>>, impl Future<Output = Result<PathBuf>>>,
    )>,
) -> Result<Vec<MountBackup>> {
    let mut backups = vec![];
    for (mount, result) in backup_results {
        match result.await {
            Ok(path) => {
                log::info!("Successfully backed up to {}", path.display());
                backups.push(MountBackup { path, mount })
            }
            Err(e) => return Err(e),
        }
    }
    Ok(backups)
}

/// Write container backup json to file
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `container_backup` - Container backup info
/// * `output` - Directory relative to `backup_mount` to write file
/// * `backup_mount` - Mount representing backup location
///
async fn write_container_backup(
    docker: &Docker,
    container_backup: ContainerBackup,
    output: PathBuf,
    backup_mount: Mount,
) -> Result<PathBuf> {
    let backup_path = output
        .as_path()
        .join(format!("{}.json", Utc::now().to_rfc3339()));
    let backup_json = base64::encode(serde_json::to_string_pretty(&container_backup)?);
    log::info!("Writing container backup file {}", backup_path.display());

    let log_prefix = format!("backup container {}", container_backup.name);
    let mounted_backup_path = format!("/backup/{}", backup_path.as_path().to_str().unwrap());
    let args = vec![
        "write",
        "--file",
        &mounted_backup_path,
        "--contents",
        &backup_json,
        "--encoded",
    ];

    match run_dockyard_command(docker, Some(vec![backup_mount]), args).await {
        Ok((exit_code, logs)) => {
            handle_container_output(exit_code, &log_prefix, &logs).map(|_| backup_path)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod test {
    use std::fs;
    use std::fs::{create_dir, read_to_string};
    use std::io::Write;

    use flate2::read::GzDecoder;
    use log::LevelFilter;
    use simple_logger::SimpleLogger;
    use tar::Archive;
    use tempfile::TempDir;

    use super::*;
    use crate::container::{check_image, get_backup_directory_mount};
    use bollard::container::{
        Config, CreateContainerOptions, KillContainerOptions, RemoveContainerOptions,
        StartContainerOptions,
    };
    use bollard::models::MountTypeEnum;
    use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
    use tokio::runtime::Runtime;
    use uuid::Uuid;

    #[test]
    fn backup_file_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let working_dir = TempDir::new().unwrap();
        let input = working_dir.path().join("input");
        let output = working_dir.path().join("output");
        let contents = "I am some contents";
        File::create(&input)
            .unwrap()
            .write_all(contents.as_bytes())
            .unwrap();
        create_dir(&output).unwrap();

        let created = backup_directory(input.to_str().unwrap(), output.to_str().unwrap()).unwrap();
        assert_eq!(
            read_to_string(output.join(created)).unwrap(),
            contents.to_string()
        );
    }

    #[test]
    fn backup_directory_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let working_dir = TempDir::new().unwrap();
        let input = working_dir.path().join("input");
        let output = working_dir.path().join("output");
        create_dir(&input).unwrap();
        create_dir(&output).unwrap();

        for i in 0..100 {
            let mut f = File::create(Path::join(&input, i.to_string())).unwrap();
            f.write_all(format!("Backup test data {}", i).as_bytes())
                .unwrap();
        }

        let created = backup_directory(input.to_str().unwrap(), output.to_str().unwrap()).unwrap();
        let tar_file = File::open(output.join(created)).unwrap();

        let tar = GzDecoder::new(tar_file);
        let mut archive = Archive::new(tar);
        let scratch = working_dir.path().join("scratch");
        create_dir(&scratch).unwrap();

        archive.unpack(&scratch).unwrap();

        let mut count = 0;
        for maybe_entry in fs::read_dir(&scratch).unwrap() {
            let entry = maybe_entry.unwrap();
            let num = entry.file_name();
            count += 1;
            assert_eq!(
                fs::read_to_string(entry.path()).unwrap(),
                format!("Backup test data {}", num.to_str().unwrap())
            );
        }
        assert_eq!(count, 100);
    }

    #[test]
    fn backup_directory_bad_paths_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let error = backup_directory("/tmp/one/bad", "/tmp/two/bad").unwrap_err();
        assert_eq!(error.to_string(), "No such file or directory (os error 2)")
    }

    #[test]
    fn backup_volume_to_directory_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let test_id = Uuid::new_v4().to_string();
        let volume_name = format!("backup_test_volume_{}", test_id);
        let mut rt = Runtime::new().unwrap();
        let docker = Docker::connect_with_unix_defaults().unwrap();

        // Setup
        rt.block_on(docker.create_volume(CreateVolumeOptions {
            name: &volume_name,
            driver: &"local".to_string(),
            driver_opts: Default::default(),
            labels: Default::default(),
        }))
        .unwrap();

        let working_dir = TempDir::new().unwrap();
        let output = Path::join(working_dir.path(), "output");
        create_dir(&output).unwrap();
        let relative = rt
            .block_on(backup_volume(
                &docker,
                volume_name,
                get_backup_directory_mount(output.to_str().unwrap().to_string()),
            ))
            .unwrap();
        assert!(&output.join(relative).exists());
    }

    #[test]
    fn backup_container_to_directory_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let test_id = Uuid::new_v4().to_string();
        let volume_name = format!("backup_test_volume_{}", test_id);
        let container_name = format!("backup_test_container_{}", test_id);
        let mut rt = Runtime::new().unwrap();
        let docker = Docker::connect_with_unix_defaults().unwrap();

        // Setup
        rt.block_on(async {
            &docker
                .create_volume(CreateVolumeOptions {
                    name: volume_name.as_str(),
                    driver: &"local".to_string(),
                    driver_opts: Default::default(),
                    labels: Default::default(),
                })
                .await
                .unwrap();
            let mounts = vec![Mount {
                target: Some("/volume".to_string()),
                source: Some(volume_name.clone()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }];
            create_and_start_container(&docker, &container_name, mounts)
                .await
                .unwrap();
        });

        let working_dir = TempDir::new().unwrap();
        let output = Path::join(working_dir.path(), "output");
        create_dir(&output).unwrap();

        let relative_path = rt
            .block_on(backup_container(
                &docker,
                &container_name,
                get_backup_directory_mount(output.to_str().unwrap().to_string()),
                &HashSet::new(),
            ))
            .unwrap();
        let absolute = &output.join(relative_path);
        assert!(&absolute.exists());
        let backup_string = fs::read_to_string(&absolute).unwrap();
        let backup: ContainerBackup = serde_json::from_str(&backup_string).unwrap();
        assert_eq!(backup.name, container_name);
        assert_eq!(backup.mounts.len(), 1);
        let volume_archive = Path::new(&backup.mounts.first().unwrap().path);
        assert!(&output.join(volume_archive).exists());

        // Cleanup
        rt.block_on(cleanup_container_and_volumes(&docker, &container_name))
            .unwrap();
    }

    async fn cleanup_container_and_volumes(docker: &Docker, name: &str) -> Result<()> {
        let mounts = docker
            .inspect_container(name, None::<InspectContainerOptions>)
            .await?
            .mounts
            .unwrap();
        docker
            .kill_container(name, None::<KillContainerOptions<String>>)
            .await?;
        docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;
        for mount in mounts {
            docker
                .remove_volume(
                    &mount.name.unwrap(),
                    Some(RemoveVolumeOptions { force: true }),
                )
                .await?;
        }
        Ok(())
    }

    async fn create_and_start_container(
        docker: &Docker,
        name: &str,
        mounts: Vec<Mount>,
    ) -> Result<()> {
        let image = "alpine:latest";
        check_image(docker, image).await.unwrap();
        docker
            .create_container(
                Some(CreateContainerOptions { name }),
                Config {
                    image: Some(image),
                    cmd: Some(vec!["tail", "-f", "/dev/null"]),
                    host_config: Some(HostConfig {
                        mounts: Some(mounts),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await?;
        docker
            .start_container(name, None::<StartContainerOptions<String>>)
            .await?;
        Ok(())
    }
}
