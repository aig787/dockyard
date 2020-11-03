use crate::backup::backup_container;
use crate::cleanup::get_all_containers;
use anyhow::Result;
use bollard::models::Mount;
use bollard::Docker;
use chrono::Utc;
use cron::Schedule;
use std::str::FromStr;
use tokio::time;

pub const ENABLED_LABEL: &str = "com.github.aig787.dockyard.enabled";
const ENABLED_VALUE: &str = "true";

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
        .filter(|container| {
            let dockyard_enabled = match container.labels.as_ref() {
                Some(labels) => labels
                    .get(ENABLED_LABEL)
                    .map(|s| s.as_str())
                    .unwrap_or(ENABLED_VALUE),
                None => ENABLED_VALUE,
            };
            if dockyard_enabled == "false" {
                log::info!(
                    "Ignoring container {}",
                    container.names.as_ref().unwrap().first().unwrap()
                );
                false
            } else {
                true
            }
        })
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
