use std::fs::{create_dir_all, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bollard::container::InspectContainerOptions;
use bollard::Docker;
use bollard::models::{HostConfig, Mount, MountPoint, ContainerConfig, ContainerInspectResponse, MountTypeEnum};
use chrono::Utc;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::collections::HashMap;
use crate::container::{run_dockyard_command, handle_container_output};
use futures::future::*;

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
pub fn backup_directory(name: &str, input: &str, output: &str) -> Result<PathBuf> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);
    let archive_path = output_path.join(name);
    create_dir_all(archive_path.parent().unwrap())?;
    log::info!("Backing up directory {} to {}", input_path.display(), archive_path.display());
    let archive = File::create(&archive_path).with_context(|| format!("Unable to create file {}", archive_path.display()))?;
    let enc = GzEncoder::new(archive, Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("", input_path)
        .with_context(|| format!("Failed to create tarball {} from {}", archive_path.display(), input))?;
    Ok(archive_path)
}

pub async fn backup_directory_to_mount(docker: &Docker, name: String, input: String, output: String, mount: Mount) -> Result<PathBuf> {
    log::info!("Backing up directory {} to {} on {}", &input, output, mount.source.as_ref().unwrap());
    let mounted_input = Path::new("/input");
    let mounted_output = Path::new(mount.target.as_ref().unwrap()).join(&output);
    let log_prefix = format!("backup directory {}", &input);
    let input_mount = Mount {
        source: Some(input),
        target: Some("/input".to_string()),
        typ: Some(MountTypeEnum::BIND),
        ..Default::default()
    };
    let args = vec!["backup", "directory", "--name", &name, mounted_input.to_str().unwrap(), mounted_output.to_str().unwrap()];
    let (exit_code, logs) = run_dockyard_command(docker, Some(vec![input_mount, mount]), args).await?;
    handle_container_output(exit_code, &log_prefix, &logs).map(|_| Path::new(&output).join(name))
}

/// Back up volume
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `volume` - Name of volume to back up
/// * `backup_mount` - Mount of backup destination
///
pub async fn backup_volume(docker: &Docker, volume: String, backup_mount: Mount) -> Result<PathBuf> {
    let mounts = vec![
        Mount {
            source: Some(volume.to_string()),
            target: Some("/volume".to_string()),
            typ: Some(MountTypeEnum::VOLUME),
            ..Default::default()
        }, backup_mount
    ];
    let output = Path::new("dockyard/volumes").join(&volume);
    log::info!("Backing up volume {} to {}", &volume, output.display());
    let archive = output.join(format!("{}.tgz", Utc::now().to_rfc3339()));
    let args = vec!["backup", "directory", "--name",
                    &archive.to_str().unwrap(), "/volume", "/backup"];
    let log_prefix = format!("backup volume {}", &volume);
    match run_dockyard_command(docker, Some(mounts), args).await {
        Ok((exit_code, logs)) =>
            handle_container_output(exit_code, &log_prefix, &logs).map(|_| archive),
        Err(e) => Err(e)
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
pub async fn backup_container(docker: &Docker, container_name: &str, backup_mount: Mount, volumes: Option<Vec<String>>) -> Result<PathBuf> {
    let output = Path::new("dockyard/containers").join(container_name);
    let (info, mounts) = get_container_info(docker, container_name, volumes).await?;
    let mut mount_backup_processes = vec![];
    for mp in mounts {
        if mp.typ.as_ref().unwrap() == "bind" {
            let output = format!("dockyard/binds/{}", mp.destination.as_ref().unwrap().replace("/", ":"));
            let archive = format!("{}.tgz", Utc::now().to_rfc3339());
            let directory = mp.source.as_ref().unwrap().clone();
            mount_backup_processes.push((mp, Either::Left(backup_directory_to_mount(docker, archive, directory, output, backup_mount.clone()))));
        } else {
            let volume_name = mp.name.as_ref().unwrap().clone();
            mount_backup_processes.push((mp, Either::Right(backup_volume(docker, volume_name, backup_mount.clone()))));
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
async fn filter_mount(docker: &Docker, mount: &MountPoint) -> Result<bool> {
    match mount.typ.as_deref() {
        Some("volume") => {
            let volume_name = mount.name.as_ref().unwrap();
            let volume = docker.inspect_volume(volume_name).await?;
            match volume.options.get("type").map(String::as_str) {
                Some("nfs") | Some("nfs4") => {
                    log::info!("Ignoring network volume {}", volume_name);
                    Ok(false)
                }
                _ => Ok(true)
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

/// Assert all specified volume mounts are present
///
/// # Arguments
///
/// * `container_info` - Container inspection result
/// * `volumes` - Specific volumes to back up
///
fn validate_mounts(container_info: &ContainerInspectResponse, volumes: Vec<String>) -> Result<Vec<MountPoint>> {
    let mounts = container_info.mounts.as_ref().unwrap().iter().map(|m| {
        match &m.typ.as_deref() {
            Some("bind") => (m.destination.as_ref().unwrap().to_string(), m),
            _ => (m.name.as_ref().unwrap().to_string(), m)
        }
    }).collect::<HashMap<String, &MountPoint>>();
    let mut mountpoints = vec![];
    for volume in volumes {
        match mounts.get(&volume) {
            Some(m) => mountpoints.push((*m).clone()),
            None => return Err(anyhow!("No mount {} found", volume))
        }
    }
    Ok(mountpoints)
}

/// Return Container Inspection and mounts for container
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `container_name` - Name of container to inspect
/// * `volumes` - Optional list of volumes to retrieve
///
async fn get_container_info(docker: &Docker, container_name: &str, volumes: Option<Vec<String>>) -> Result<(ContainerInspectResponse, Vec<MountPoint>)> {
    let container_info = docker.inspect_container(&container_name, None::<InspectContainerOptions>).await?;
    let mounts = match volumes {
        Some(v) => validate_mounts(&container_info, v)?,
        None => {
            let mut filtered_mounts = vec![];
            for mp in container_info.mounts.as_ref().unwrap() {
                if filter_mount(docker, mp).await? {
                    filtered_mounts.push(mp.clone())
                }
            }
            filtered_mounts
        }
    };
    Ok((container_info, mounts))
}

/// Await volume backups and return a MountBackup for each
///
/// # Arguments
///
/// * `backup_results` - List of volume backup results
///
async fn validate_process_results(backup_results: Vec<(MountPoint, Either<impl Future<Output=Result<PathBuf>>, impl Future<Output=Result<PathBuf>>>)>) -> Result<Vec<MountBackup>> {
    let mut backups = vec![];
    for (mount, result) in backup_results {
        match result.await {
            Ok(path) => {
                log::info!("Successfully backed up to {}", path.display());
                backups.push(MountBackup { path, mount })
            }
            Err(e) => return Err(e)
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
async fn write_container_backup(docker: &Docker, container_backup: ContainerBackup, output: PathBuf, backup_mount: Mount) -> Result<PathBuf> {
    let backup_path = output.as_path().join(format!("{}.json", Utc::now().to_rfc3339()));
    let backup_json = base64::encode(serde_json::to_string_pretty(&container_backup)?);
    log::info!("Writing container backup file {}", backup_path.display());

    let log_prefix = format!("backup container {}", container_backup.name);
    let mounted_backup_path = format!("/backup/{}", backup_path.as_path().to_str().unwrap());
    let args = vec!["write", "--file", &mounted_backup_path, "--contents", &backup_json, "--encoded"];

    match run_dockyard_command(docker, Some(vec![backup_mount]), args).await {
        Ok((exit_code, logs)) => handle_container_output(exit_code, &log_prefix, &logs).map(|_| backup_path),
        Err(e) => Err(e)
    }
}

#[cfg(test)]
mod test {
    use std::fs::create_dir;
    use std::fs;
    use std::io::Write;

    use flate2::read::GzDecoder;
    use log::LevelFilter;
    use simple_logger::SimpleLogger;
    use tar::Archive;
    use tempfile::TempDir;

    use super::*;
    use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
    use tokio::runtime::Runtime;
    use bollard::container::{KillContainerOptions, RemoveContainerOptions, CreateContainerOptions, Config, StartContainerOptions};
    use bollard::models::MountTypeEnum;
    use uuid::Uuid;
    use crate::container::get_backup_directory_mount;

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
            f.write_all(format!("Backup test data {}", i).as_bytes()).unwrap();
        }

        let created = backup_directory("test.tgz", input.to_str().unwrap(), output.to_str().unwrap()).unwrap();
        let tar_file = File::open(created).unwrap();

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
            assert_eq!(fs::read_to_string(entry.path()).unwrap(), format!("Backup test data {}", num.to_str().unwrap()));
        }
        assert_eq!(count, 100);
    }

    #[test]
    fn backup_directory_bad_paths_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let error = backup_directory("test.tgz", "/tmp/one/bad", "/tmp/two/bad").unwrap_err();
        assert_eq!(error.to_string(), "Failed to create tarball /tmp/two/bad/test.tgz from /tmp/one/bad");
        assert_eq!(error.root_cause().to_string(), "No such file or directory (os error 2)")
    }

    #[test]
    fn backup_volume_to_directory_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let test_id = Uuid::new_v4().to_string();
        let volume_name = format!("backup_test_volume_{}", test_id);
        let mut rt = Runtime::new().unwrap();
        let docker = Docker::connect_with_unix_defaults().unwrap();

        // Setup
        rt.block_on(
            docker.create_volume(CreateVolumeOptions {
                name: &volume_name,
                driver: &"local".to_string(),
                driver_opts: Default::default(),
                labels: Default::default(),
            })
        ).unwrap();

        let working_dir = TempDir::new().unwrap();
        let output = Path::join(working_dir.path(), "output");
        create_dir(&output).unwrap();
        let relative = rt.block_on(
            backup_volume(&docker, volume_name, get_backup_directory_mount(output.to_str().unwrap().to_string()))
        ).unwrap();
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
            &docker.create_volume(CreateVolumeOptions {
                name: volume_name.as_str(),
                driver: &"local".to_string(),
                driver_opts: Default::default(),
                labels: Default::default(),
            }).await.unwrap();
            let mounts = vec![Mount {
                target: Some("/volume".to_string()),
                source: Some(volume_name.clone()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }];
            create_and_start_container(&docker, &container_name, mounts).await.unwrap();
        });

        let working_dir = TempDir::new().unwrap();
        let output = Path::join(working_dir.path(), "output");
        create_dir(&output).unwrap();

        let relative_path = rt.block_on(backup_container(&docker, &container_name, get_backup_directory_mount(output.to_str().unwrap().to_string()), None)).unwrap();
        let absolute = &output.join(relative_path);
        assert!(&absolute.exists());
        let backup_string = fs::read_to_string(&absolute).unwrap();
        let backup: ContainerBackup = serde_json::from_str(&backup_string).unwrap();
        assert_eq!(backup.name, container_name);
        assert_eq!(backup.mounts.len(), 1);
        let volume_archive = Path::new(&backup.mounts.first().unwrap().path);
        assert!(&output.join(volume_archive).exists());

        // Cleanup
        rt.block_on(cleanup_container_and_volumes(&docker, &container_name)).unwrap();
    }

    async fn cleanup_container_and_volumes(docker: &Docker, name: &str) -> Result<()> {
        let mounts = docker.inspect_container(name, None::<InspectContainerOptions>).await?.mounts.unwrap();
        docker.kill_container(name, None::<KillContainerOptions<String>>).await?;
        docker.remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() })).await?;
        for mount in mounts {
            docker.remove_volume(&mount.name.unwrap(), Some(RemoveVolumeOptions { force: true })).await?;
        }
        Ok(())
    }

    async fn create_and_start_container(docker: &Docker, name: &str, mounts: Vec<Mount>) -> Result<()> {
        docker.create_container(
            Some(CreateContainerOptions { name }),
            Config {
                image: Some("alpine:latest"),
                cmd: Some(vec!["tail", "-f", "/dev/null"]),
                host_config: Some(HostConfig {
                    mounts: Some(mounts),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ).await?;
        docker.start_container(name, None::<StartContainerOptions<String>>).await?;
        Ok(())
    }
}