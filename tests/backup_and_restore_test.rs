#[macro_use]
pub mod common;

use crate::common::*;
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, KillContainerOptions, LogOutput,
    RemoveContainerOptions, StartContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use bollard::Docker;
use dockyard::container::{get_backup_volume_mount, run_dockyard_command, set_command_verbosity};
use futures::TryStreamExt;
use log::LevelFilter;
use simple_logger::SimpleLogger;
use std::collections::HashSet;
use std::fs::{create_dir, remove_dir_all, File};
use std::io::Write;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use uuid::Uuid;

#[test]
fn backup_all_container_volumes_test() {
    set_command_verbosity(3);
    let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
    let working_dir = TempDir::new().unwrap();
    let mut rt = Runtime::new().unwrap();
    let client = Docker::connect_with_unix_defaults().unwrap();
    let test_id = Uuid::new_v4().to_string();

    let bind_input_directory = working_dir.path().join("bind-input");
    create_dir(&bind_input_directory).unwrap();
    for i in 1..5 {
        File::create(&bind_input_directory.join(i.to_string()))
            .unwrap()
            .write_all("BIND DATA".as_bytes())
            .unwrap();
    }

    let volume_name = format!("volume_{}", &test_id);
    let volume_input_directory = working_dir.path().join("volume-input");
    let volume_archive = working_dir.path().join("volume.tgz");
    create_archive(
        volume_input_directory.as_path(),
        volume_archive.as_path(),
        3,
    );

    let container_name = format!("test_container_{}", &test_id);
    let backup_name = format!("backup_{}", &test_id);

    // Create container
    rt.block_on(async {
        // Create volume
        &client
            .create_volume(CreateVolumeOptions {
                name: volume_name.clone(),
                driver: "local".to_string(),
                driver_opts: Default::default(),
                labels: Default::default(),
            })
            .await
            .unwrap();
        // Create backup volume
        &client
            .create_volume(CreateVolumeOptions {
                name: backup_name.clone(),
                driver: "local".to_string(),
                driver_opts: Default::default(),
                labels: Default::default(),
            })
            .await
            .unwrap();

        // Copy archive to init volume contents
        let (exit_code, _) = run_dockyard_command(
            &client,
            Some(vec![
                Mount {
                    target: Some("/backup.tgz".to_string()),
                    source: Some(volume_archive.to_str().unwrap().to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    ..Default::default()
                },
                Mount {
                    target: Some("/volume".to_string()),
                    source: Some(volume_name.clone()),
                    typ: Some(MountTypeEnum::VOLUME),
                    ..Default::default()
                },
            ]),
            vec!["restore", "directory", "/backup.tgz", "volume"],
        )
        .await
        .unwrap();
        assert_eq!(exit_code, 0);

        // Create container with both mounts
        &client
            .create_container(
                Some(CreateContainerOptions {
                    name: container_name.clone(),
                }),
                Config {
                    env: Some(vec!["VAR1=a", "VAR2=b"]),
                    cmd: Some(vec!["tail", "-f", "/dev/null"]),
                    image: Some("alpine:latest"),
                    working_dir: Some("/var"),
                    labels: Some(vec![("com.a.label-one", "one")].into_iter().collect()),
                    host_config: Some(HostConfig {
                        mounts: Some(vec![
                            Mount {
                                target: Some("/volume1".to_string()),
                                source: Some(bind_input_directory.to_str().unwrap().to_string()),
                                typ: Some(MountTypeEnum::BIND),
                                ..Default::default()
                            },
                            Mount {
                                target: Some("/volume2".to_string()),
                                source: Some(volume_name.clone()),
                                typ: Some(MountTypeEnum::VOLUME),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // Start container
        &client
            .start_container(&container_name, None::<StartContainerOptions<String>>)
            .await
            .unwrap();
    });

    rt.block_on(async {
        let backup_mount = get_backup_volume_mount(backup_name.clone());
        // Back up container
        let backup = dockyard::backup::backup_container(
            &client,
            &container_name,
            backup_mount.clone(),
            &HashSet::new(),
        )
        .await
        .unwrap();

        // Remove originals
        &client
            .kill_container(&container_name, None::<KillContainerOptions<String>>)
            .await
            .unwrap();
        &client
            .remove_container(&container_name, None::<RemoveContainerOptions>)
            .await
            .unwrap();
        &client
            .remove_volume(&volume_name, None::<RemoveVolumeOptions>)
            .await
            .unwrap();
        remove_dir_all(&bind_input_directory).unwrap();
        create_dir(&bind_input_directory).unwrap();

        // Restore container
        let restored_name = format!("restored_{}", container_name);
        dockyard::restore::restore_container(
            &client,
            backup.to_str().unwrap(),
            &restored_name,
            backup_mount.clone(),
        )
        .await
        .unwrap();
        &client
            .start_container(&restored_name, None::<StartContainerOptions<String>>)
            .await
            .unwrap();

        let out1 = run_in_container(&client, &restored_name, vec!["/bin/ls", "/volume1"]).await;
        assert_eq!(out1.first().unwrap(), "1\n2\n3\n4\n");
        let out2 = run_in_container(&client, &restored_name, vec!["/bin/ls", "/volume2"]).await;
        assert_eq!(out2.first().unwrap(), "0\n1\n2\n");
        let container_info = &client
            .inspect_container(&restored_name, None::<InspectContainerOptions>)
            .await
            .unwrap();
        let env = container_info
            .config
            .as_ref()
            .unwrap()
            .env
            .as_ref()
            .unwrap()
            .into_iter()
            .collect::<HashSet<_>>();
        assert!(env.contains(&"VAR1=a".to_string()));
        assert!(env.contains(&"VAR2=b".to_string()));
        assert!(container_info
            .config
            .as_ref()
            .unwrap()
            .labels
            .as_ref()
            .unwrap()
            .contains_key("com.a.label-one"));

        // Remove restored container
        &client
            .kill_container(&restored_name, None::<KillContainerOptions<String>>)
            .await
            .unwrap();
        &client
            .remove_container(&restored_name, None::<RemoveContainerOptions>)
            .await
            .unwrap();

        // Remove volumes
        &client
            .remove_volume(&backup_name, None::<RemoveVolumeOptions>)
            .await
            .unwrap();
        &client
            .remove_volume(&volume_name, None::<RemoveVolumeOptions>)
            .await
            .unwrap();
    });
}

async fn run_in_container(docker: &Docker, container_name: &str, cmd: Vec<&str>) -> Vec<String> {
    let message = docker
        .create_exec(
            container_name,
            CreateExecOptions {
                attach_stdout: Some(true),
                cmd: Some(cmd),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let exec_start = docker
        .start_exec(&message.id, None::<StartExecOptions>)
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    exec_start
        .into_iter()
        .flat_map(|x| match x {
            StartExecResults::Attached { log } => match log {
                LogOutput::StdOut { message } => {
                    Some(String::from_utf8_lossy(&message).to_string())
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
}
