use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions,
};
use bollard::models::{Volume, Mount, HostConfig};
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use bollard::Docker;
use std::path::Path;
use std::fs::{create_dir, File};
use std::io::Write;
use flate2::write::GzEncoder;
use flate2::Compression;

pub fn create_archive(input: &Path, destination: &Path, file_count: i32) {
    create_dir(input).unwrap();

    for i in 0..file_count {
        File::create(Path::join(&input, i.to_string())).unwrap()
            .write_all(format!("Test Data {}", i).as_bytes()).unwrap();
    }

    let archive = File::create(destination).unwrap();
    let enc = GzEncoder::new(archive, Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("", input).unwrap();
}

pub async fn create_container(
    client: &Docker,
    container_name: &str,
    mounts: Option<Vec<Mount>>,
) -> Result<(), bollard::errors::Error> {
    client
        .create_container(
            Some(CreateContainerOptions {
                name: container_name,
            }),
            Config {
                image: Some("nginx:latest"),
                host_config: Some(HostConfig {
                    mounts,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    client
        .start_container(container_name, None::<StartContainerOptions<String>>)
        .await
}

pub async fn create_volume(
    client: &Docker,
    volume_name: &str,
) -> Result<Volume, bollard::errors::Error> {
    client
        .create_volume(CreateVolumeOptions {
            name: volume_name,
            driver: "local",
            ..Default::default()
        })
        .await
}

pub async fn remove_volume(
    client: &Docker,
    volume_name: &str,
) -> Result<(), bollard::errors::Error> {
    client
        .remove_volume(volume_name, None::<RemoveVolumeOptions>)
        .await
}

pub async fn remove_container(
    client: &Docker,
    container_name: &str,
) -> Result<(), bollard::errors::Error> {
    client
        .stop_container(container_name, None::<StopContainerOptions>)
        .await?;
    client
        .remove_container(container_name, None::<RemoveContainerOptions>)
        .await
}
