use crate::backup::backup_container;
use crate::cleanup::get_all_containers;
use anyhow::Result;
use bollard::models::{ContainerSummaryInner, Mount};
use bollard::Docker;
use chrono::Utc;
use cron::Schedule;
use std::str::FromStr;
use tokio::time;

pub const DISABLED_LABEL: &str = "com.github.aig787.dockyard.disabled";

pub async fn backup_on_interval(docker: &Docker, cron: &str, backup_mount: Mount) -> Result<()> {
    let schedule = match Schedule::from_str(cron) {
        Ok(s) => s,
        Err(e) => return Err(anyhow!("Failed to parse cron expression {}: {}", cron, e)),
    };
    for datetime in schedule.upcoming(Utc) {
        let now = Utc::now();
        let now_epoch = now.timestamp();
        let datetime_epoch = datetime.timestamp();
        let duration = if now_epoch > datetime_epoch {
            log::warn!(
                "Scheduled time {} is after current time {}, running immediately",
                datetime,
                now
            );
            time::Duration::from_secs(0)
        } else {
            time::Duration::from_secs((datetime_epoch - now_epoch) as u64)
        };
        log::info!("Scheduling backup for {}", datetime.to_rfc2822());
        log::debug!("Sleeping for {} millis", &duration.as_millis());
        tokio::time::delay_for(duration).await;

        let res = backup_all_containers(docker, &backup_mount).await;
        if let Err(e) = res {
            return Err(e);
        }
    }
    Ok(())
}

async fn backup_all_containers(docker: &Docker, backup_mount: &Mount) -> Result<()> {
    let containers = get_all_containers(docker)
        .await?
        .into_iter()
        .filter(|container| should_back_up(container))
        .collect::<Vec<_>>();
    log::info!("Found {} running containers", containers.len());
    for container in containers {
        let container_name = container.names.unwrap();
        let container_name = container_name.first().unwrap().replace("/", "");
        let backup_location =
            backup_container(&docker, &container_name, backup_mount.clone(), None).await?;
        log::info!(
            "Successfully backed up {} to {}",
            container_name,
            backup_location.display()
        );
    }
    Ok(())
}

fn should_back_up(container_summary: &ContainerSummaryInner) -> bool {
    match &container_summary.labels {
        None => true,
        Some(labels) => {
            log::debug!(
                "Found {} with labels {:?}",
                &container_summary.names.as_ref().unwrap().first().unwrap(),
                &container_summary.labels.as_ref().unwrap()
            );
            !labels.contains_key(DISABLED_LABEL)
        }
    }
}
