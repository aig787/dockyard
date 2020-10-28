use anyhow::{Context, Result};
use std::path::Path;
use std::fs::{File, create_dir_all};
use flate2::read::GzDecoder;
use tar::Archive;
use bollard::Docker;
use bollard::models::{Mount, MountTypeEnum};
use bollard::volume::CreateVolumeOptions;
use crate::container::{run_dockyard_command, handle_container_output};
use crate::backup::ContainerBackup;
use bollard::container::{CreateContainerOptions, Config};
use crate::file::decode_b64;
use futures::future::Either;
use bollard::image::CreateImageOptions;
use futures::TryStreamExt;

pub fn restore_directory(archive: &str, output: &str) -> Result<()> {
    log::info!("Restoring {} to {}", archive, output);
    let output_path = Path::new(output);
    let tar_file = File::open(Path::new(archive))?;
    let tar = GzDecoder::new(tar_file);
    let mut archive = Archive::new(tar);
    create_dir_all(&output_path)?;
    archive.unpack(&output_path)?;
    Ok(())
}

pub async fn restore_directory_from_mount(docker: &Docker, archive: String, backup_mount: Mount, directory: String) -> Result<()> {
    log::info!("Restoring directory {} from {}", directory, archive);
    let log_prefix = format!("restore directory {}", directory);
    let mounted_backup = format!("{}/{}", &backup_mount.target.as_ref().unwrap(), archive);
    let mounts = Some(vec![
        backup_mount,
        Mount {
            target: Some("/output".to_string()),
            source: Some(directory.to_string()),
            typ: Some(MountTypeEnum::BIND),
            ..Default::default()
        },
    ]);
    let cmd = vec!["restore", "directory", &mounted_backup, "/output"];
    let (exit_code, logs) = run_dockyard_command(docker, mounts, cmd).await?;
    handle_container_output(exit_code, &log_prefix, &logs)
}

pub async fn restore_volume(docker: &Docker, archive: String, backup_mount: Mount, volume_mount: Mount) -> Result<()> {
    log::info!("Restoring volume {} from {}", volume_mount.source.as_ref().unwrap(), archive);
    docker.create_volume(CreateVolumeOptions {
        name: volume_mount.source.as_ref().unwrap().to_string(),
        driver: "local".to_string(),
        driver_opts: Default::default(),
        labels: Default::default(),
    }).await?;
    let log_prefix = format!("restore volume {}", volume_mount.source.as_ref().unwrap());
    let mounted_backup = format!("{}/{}", &backup_mount.target.as_ref().unwrap(), archive);
    let volume_dir = volume_mount.target.as_ref().unwrap().to_string();
    let cmd = vec!["restore", "directory", &mounted_backup, &volume_dir];
    let mounts = Some(vec![backup_mount, volume_mount]);
    let (exit_code, logs) = run_dockyard_command(docker, mounts, cmd).await?;
    handle_container_output(exit_code, &log_prefix, &logs)
}

pub async fn restore_container(docker: &Docker, backup_file: &str, container: &str, backup_mount: Mount) -> Result<()> {
    log::info!("Restoring container {} from {}", container, backup_file);
    let mounted_backup = format!("/backup/{}", backup_file);
    let (exit_code, logs) = run_dockyard_command(
        docker,
        Some(vec![backup_mount.clone()]),
        vec!["cat", "--encoded", "-f", &mounted_backup],
    ).await?;
    if logs.is_empty() {
        return Err(anyhow!("Found empty file"));
    }
    let log_prefix = format!("restore container {}", container);
    handle_container_output(exit_code, &log_prefix, &logs[0..logs.len() - 1])?;
    let container_backup = decode_b64(logs.last().unwrap().to_string().trim())?;
    let container_backup: ContainerBackup = serde_json::from_str(&container_backup)?;
    let mut mount_restore_processes = vec![];
    for mb in container_backup.mounts {
        let archive_path = mb.path.to_str().unwrap().to_string();
        if mb.mount.typ.unwrap() == "bind" {
            let directory = mb.mount.source.unwrap();
            let f = restore_directory_from_mount(docker, archive_path, backup_mount.clone(), directory.clone());
            mount_restore_processes.push((directory, Either::Left(f)));
        } else {
            let volume = mb.mount.name.unwrap();
            let volume_mount = Mount {
                target: Some("/volume".to_string()),
                source: Some(volume.clone()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            };
            let f = restore_volume(docker, archive_path, backup_mount.clone(), volume_mount);
            mount_restore_processes.push((volume, Either::Right(f)));
        }
    }
    for (name, res) in mount_restore_processes {
        res.await.with_context(|| format!("Failed to restore mount {}", &name))?;
        log::info!("Successfully restored mount {}", &name)
    }

    let image = container_backup.container_config.image.unwrap();
    docker.create_image(Some(CreateImageOptions { from_image: image.as_str(), ..Default::default() }), None, None)
        .try_collect::<Vec<_>>()
        .await?;

    let container_config = Config {
        hostname: container_backup.container_config.hostname,
        domainname: container_backup.container_config.domainname,
        user: container_backup.container_config.user,
        attach_stdin: container_backup.container_config.attach_stdin,
        attach_stdout: container_backup.container_config.attach_stdout,
        attach_stderr: container_backup.container_config.attach_stderr,
        exposed_ports: container_backup.container_config.exposed_ports,
        tty: container_backup.container_config.tty,
        open_stdin: container_backup.container_config.open_stdin,
        stdin_once: container_backup.container_config.stdin_once,
        env: container_backup.container_config.env,
        cmd: container_backup.container_config.cmd,
        healthcheck: container_backup.container_config.healthcheck,
        args_escaped: container_backup.container_config.args_escaped,
        image: Some(image),
        volumes: container_backup.container_config.volumes,
        working_dir: container_backup.container_config.working_dir,
        entrypoint: container_backup.container_config.entrypoint,
        network_disabled: container_backup.container_config.network_disabled,
        mac_address: container_backup.container_config.mac_address,
        on_build: container_backup.container_config.on_build,
        labels: container_backup.container_config.labels,
        stop_signal: container_backup.container_config.stop_signal,
        stop_timeout: container_backup.container_config.stop_timeout,
        shell: container_backup.container_config.shell,
        host_config: Some(container_backup.host_config),
        ..Default::default()
    };

    docker.create_container(Some(CreateContainerOptions {
        name: container
    }), container_config).await?;
    log::info!("Successfully restored container {}", container);
    Ok(())
}


#[cfg(test)]
mod test {
    use super::*;
    use simple_logger::SimpleLogger;
    use log::LevelFilter;
    use tempfile::TempDir;
    use std::fs::{create_dir, read_to_string, read_dir};
    use std::io::Write;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::path::PathBuf;
    use tokio::runtime::Runtime;
    use bollard::models::{MountPoint, ContainerConfig, HostConfig};
    use crate::backup::MountBackup;
    use bollard::container::{InspectContainerOptions, RemoveContainerOptions};
    use uuid::Uuid;
    use crate::container::{get_backup_directory_mount, run_docker_command};

    #[test]
    fn restore_directory_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let working_dir = TempDir::new().unwrap();
        let archive_path = create_archive(&working_dir);
        let output = Path::join(&working_dir.path(), "output");
        create_dir(&output).unwrap();
        restore_directory(&archive_path.to_str().unwrap(), &output.to_str().unwrap()).unwrap();
    }

    #[test]
    fn restore_volume_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let test_id = Uuid::new_v4().to_string();
        let working_dir = TempDir::new().unwrap();
        let archive_path = create_archive(&working_dir);
        let volume_contents = working_dir.path().join("volume-contents");

        let mut rt = Runtime::new().unwrap();
        let docker = Docker::connect_with_unix_defaults().unwrap();

        rt.block_on(async {
            let volume_name = format!("volume_{}", test_id);
            &docker.create_volume(CreateVolumeOptions {
                name: volume_name.clone(),
                driver: "local".to_string(),
                driver_opts: Default::default(),
                labels: Default::default(),
            });
            let volume_mount = Mount {
                target: Some("/volume".to_string()),
                source: Some(volume_name.clone()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            };
            restore_volume(
                &docker,
                archive_path.strip_prefix(working_dir.path()).unwrap().to_str().unwrap().to_string(),
                Mount {
                    target: Some("/backup".to_string()),
                    source: Some(working_dir.path().to_str().unwrap().to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    ..Default::default()
                }, volume_mount.clone()).await.unwrap();
            copy_from_volume(&docker, &volume_name, volume_contents.to_str().unwrap()).await.unwrap();
        });

        let mut count = 0;
        for maybe_entry in read_dir(volume_contents).unwrap() {
            let entry = maybe_entry.unwrap();
            let num = entry.file_name();
            count += 1;
            assert_eq!(read_to_string(entry.path()).unwrap(), format!("Restore test data {}", num.to_str().unwrap()));
        }
        assert_eq!(count, 100);
    }

    #[test]
    fn restore_container_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let working_dir = TempDir::new().unwrap();
        let volume_restore_path = Path::join(working_dir.path(), "volume");
        create_dir_all(&volume_restore_path).unwrap();
        let archive_path = create_archive(&working_dir);
        let container_name = format!("restore_test_{}", Uuid::new_v4());
        let backup_name = "backup.json";
        let source = Some(volume_restore_path.to_str().unwrap().to_string());
        let typ = Some("bind".to_string());
        let name = Some("volume".to_string());
        let destination = Some("/volume".to_string());
        let driver = Some("local".to_string());
        let mount_backup = MountBackup {
            path: PathBuf::from(archive_path.strip_prefix(&working_dir).unwrap()),
            mount: MountPoint { name: name.clone(), typ: typ.clone(), source: source.clone(), destination: destination.clone(), driver: driver.clone(), ..Default::default() },
        };
        let mount = Mount {
            target: destination.clone(),
            source: source.clone(),
            typ: Some(MountTypeEnum::BIND),
            ..Default::default()
        };
        let container_backup = ContainerBackup {
            name: container_name.clone(),
            container_config: ContainerConfig {
                cmd: Some(vec!["tail".to_string(), "-f".to_string(), "/dev/null".to_string()]),
                image: Some("nginx:latest".to_string()),
                entrypoint: None,
                ..Default::default()
            },
            host_config: HostConfig { mounts: Some(vec![mount]), ..Default::default() },
            mounts: vec![mount_backup],
        };
        let backup_path = working_dir.path().join(backup_name);
        File::create(&backup_path).unwrap().write_all(serde_json::to_string(&container_backup).unwrap().as_bytes()).unwrap();

        let mut rt = Runtime::new().unwrap();
        let docker = Docker::connect_with_unix_defaults().unwrap();
        let inspection = rt.block_on(async {
            restore_container(&docker, backup_name, container_name.as_str(), get_backup_directory_mount(working_dir.path().to_str().unwrap().to_string())).await.unwrap();
            let inspection = &docker.inspect_container(&container_name, None::<InspectContainerOptions>).await.unwrap();
            inspection.clone()
        });
        assert_eq!(inspection.name.as_ref().unwrap(), &format!("/{}", container_backup.name));
        let inspection_mounts = inspection.mounts.as_ref().unwrap();
        assert_eq!(inspection_mounts.len(), 1);
        let inspection_mount = inspection_mounts.first().unwrap();
        assert_eq!(inspection_mount.typ.as_ref().unwrap(), &typ.unwrap());
        assert!(inspection_mount.source.as_ref().unwrap().ends_with(&source.unwrap()));
        assert_eq!(inspection_mount.destination.as_ref().unwrap(), &destination.unwrap());
        rt.block_on(async {
            docker.remove_container(&container_name, None::<RemoveContainerOptions>).await.unwrap();
        });
    }

    fn create_archive(working_dir: &TempDir) -> PathBuf {
        let input = Path::join(working_dir.path(), "input");
        create_dir(input.as_path()).unwrap();

        for i in 0..100 {
            let mut f = File::create(Path::join(&input, i.to_string())).unwrap();
            f.write_all(format!("Restore test data {}", i).as_bytes()).unwrap();
        }

        let archive_path = Path::join(working_dir.path(), "archive.tgz");
        let archive = File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(archive, Compression::default());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all("", input.as_path()).unwrap();
        archive_path
    }

    async fn copy_from_volume(docker: &Docker, volume: &str, destination: &str) -> Result<()> {
        let mounts = vec![
            Mount {
                source: Some(volume.to_string()),
                target: Some("/source".to_string()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }, Mount {
                source: Some(destination.to_string()),
                target: Some("/destination".to_string()),
                typ: Some(MountTypeEnum::BIND),
                ..Default::default()
            }
        ];
        create_dir_all(Path::new(destination)).unwrap();
        let cmd = vec!["/bin/cp", "-r", "/source/.", "/destination"];
        let container_name = format!("copy_from_volume_{}", Uuid::new_v4());
        let (exit_code, logs) = run_docker_command(docker, &container_name, "alpine:latest", Some(mounts), cmd, None).await?;
        handle_container_output(exit_code, "copy from volume", &logs)
    }
}