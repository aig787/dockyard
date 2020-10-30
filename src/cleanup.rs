use crate::container::PID_LABEL;
use anyhow::Result;
use bollard::container::{KillContainerOptions, ListContainersOptions, RemoveContainerOptions};
use bollard::models::ContainerSummaryInner;
use bollard::Docker;
use std::collections::HashMap;
use std::process;

/// Stop and remove all dockyard containers
///
/// # Arguments
///
/// * `docker` - Docker client
///
pub async fn cleanup_dockyard_containers(docker: &Docker) -> Result<()> {
    stop_and_remove_containers(docker, get_dockyard_containers(docker).await?).await
}

/// Stop and remove all child containers
///
/// # Arguments
///
/// * `docker` - Docker client
///
pub async fn cleanup_child_containers(docker: &Docker) -> Result<()> {
    stop_and_remove_containers(docker, get_containers_by_pid(docker, process::id()).await?).await
}

/// Stop and remove specified containers
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `containers` - List of containers to stop
///
async fn stop_and_remove_containers(
    docker: &Docker,
    containers: Vec<ContainerSummaryInner>,
) -> Result<()> {
    for container in containers {
        let id = container.id.unwrap();
        let names = container.names.unwrap();
        let name = names.first().unwrap();
        let state = container.state.unwrap().to_lowercase();
        if state == "running" {
            log::info!("Killing container {}", &name);
            docker
                .kill_container(&id, None::<KillContainerOptions<String>>)
                .await?;
        }
        log::info!("Removing container {}", &name);
        docker
            .remove_container(&id, None::<RemoveContainerOptions>)
            .await?;
    }
    Ok(())
}

/// Return all containers started by dockyard process with pid
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `pid` - PID of dockyard process
///
async fn get_containers_by_pid(docker: &Docker, pid: u32) -> Result<Vec<ContainerSummaryInner>> {
    get_containers_by_label(docker, vec![format!("{}={}", PID_LABEL, pid).as_str()]).await
}

/// Return all containers started by dockyard
///
/// # Arguments
///
/// * `docker` - Docker client
///
async fn get_dockyard_containers(docker: &Docker) -> Result<Vec<ContainerSummaryInner>> {
    get_containers_by_label(docker, vec![PID_LABEL]).await
}

/// Return all containers with labels
///
/// # Arguments
///
/// * `docker` - Docker client
/// * `labels` - Labels to filter by
///
async fn get_containers_by_label(
    docker: &Docker,
    labels: Vec<&str>,
) -> Result<Vec<ContainerSummaryInner>> {
    match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: vec![("label", labels)]
                .into_iter()
                .collect::<HashMap<&str, Vec<&str>>>(),
            ..Default::default()
        }))
        .await
    {
        Ok(r) => Ok(r),
        Err(e) => Err(anyhow!("Failed getting containers by label: {}", e)),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::container::download_image;
    use bollard::container::{Config, CreateContainerOptions};
    use log::LevelFilter;
    use simple_logger::SimpleLogger;
    use std::collections::HashSet;
    use tokio::runtime::Runtime;
    use uuid::Uuid;

    #[test]
    fn get_containers_by_pid_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let mut rt = Runtime::new().unwrap();
        let docker = Docker::connect_with_unix_defaults().unwrap();

        let pid: u32 = rand::random();
        // create container with label
        let id = rt.block_on(async {
            download_image(&docker, "hello-world:linux").await.unwrap();
            create_hello_container(&docker, pid).await.unwrap()
        });

        let containers_for_pid = rt.block_on(get_containers_by_pid(&docker, pid)).unwrap();
        assert_eq!(containers_for_pid.len(), 1);
        assert_eq!(
            containers_for_pid.first().unwrap().id.as_ref().unwrap(),
            &id
        );
        rt.block_on(async {
            &docker
                .remove_container(id.as_str(), None::<RemoveContainerOptions>)
                .await
                .unwrap();
        });
    }

    #[test]
    fn get_dockyard_containers_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let mut rt = Runtime::new().unwrap();
        let docker = Docker::connect_with_unix_defaults().unwrap();

        let ids = rt.block_on(async {
            download_image(&docker, "hello-world:linux").await.unwrap();
            let id1 = create_hello_container(&docker, rand::random())
                .await
                .unwrap();
            let id2 = create_hello_container(&docker, rand::random())
                .await
                .unwrap();
            vec![id1, id2].into_iter().collect::<HashSet<String>>()
        });

        let dockyard_containers = rt.block_on(async {
            get_dockyard_containers(&docker)
                .await
                .unwrap()
                .into_iter()
                .filter(|info| ids.contains(info.id.as_ref().unwrap()))
                .collect::<Vec<_>>()
        });
        assert_eq!(dockyard_containers.len(), 2);
        rt.block_on(stop_and_remove_containers(&docker, dockyard_containers))
            .unwrap();
    }

    async fn create_hello_container(
        docker: &Docker,
        pid: u32,
    ) -> Result<String, bollard::errors::Error> {
        let id = docker
            .create_container(
                Some(CreateContainerOptions {
                    name: format!("cleanup_test_{}", Uuid::new_v4()),
                }),
                Config {
                    image: Some("hello-world:linux"),
                    labels: Some(
                        vec![(PID_LABEL, pid.to_string().as_str())]
                            .into_iter()
                            .collect::<HashMap<&str, &str>>(),
                    ),
                    ..Default::default()
                },
            )
            .await?
            .id;
        Ok(id)
    }
}
