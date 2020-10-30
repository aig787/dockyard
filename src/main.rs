use std::process::exit;

#[macro_use]
extern crate clap;

use anyhow::Result;
use bollard::Docker;
use clap::{App, ArgMatches};
use dockyard::backup::{backup_container, backup_directory, backup_volume};
use dockyard::cleanup::{cleanup_child_containers, cleanup_dockyard_containers};
use dockyard::container::{
    get_backup_directory_mount, get_backup_volume_mount, get_bind_mount, get_volume_mount,
    set_command_verbosity,
};
use dockyard::file::{decode_and_write_file, read_and_encode_file, read_file, write_file};
use dockyard::restore::{restore_container, restore_directory, restore_volume};
use log::LevelFilter;
use simple_logger::SimpleLogger;
use tokio::runtime::Runtime;

fn main() {
    let yaml = load_yaml!("cli.yml");
    let args = App::from_yaml(yaml)
        .version(env!("VERGEN_SEMVER"))
        .get_matches();

    let verbosity = args.occurrences_of("verbose");
    set_command_verbosity(verbosity as u8);
    let (global_level, module_level) = match verbosity {
        0 => (LevelFilter::Warn, LevelFilter::Info),
        1 => (LevelFilter::Warn, LevelFilter::Debug),
        2 => (LevelFilter::Info, LevelFilter::Trace),
        _ => (LevelFilter::Debug, LevelFilter::Trace),
    };

    SimpleLogger::new()
        .with_module_level("dockyard", module_level)
        .with_level(global_level)
        .init()
        .unwrap();

    ctrlc::set_handler(move || {
        log::info!("Received Ctrl-C, stopping and removing all child containers");
        let (mut rt, client) = init_docker().unwrap();
        match &rt.block_on(cleanup_child_containers(&client)) {
            Ok(_) => {
                log::info!("Successfully cleaned up child containers");
                exit(0)
            }
            Err(e) => {
                log::error!("Error cleaning up child containers: {}", e);
                exit(1)
            }
        }
    })
    .expect("Error setting Ctrl-C handler");

    let (mut rt, docker) = init_docker().unwrap();
    let result = match args.subcommand() {
        ("cleanup", _) => {
            log::info!("Cleaning up all dockyard containers");
            rt.block_on(cleanup_dockyard_containers(&docker)).map(|_| {
                log::info!("Successfully cleaned up all dockyard containers");
                0
            })
        }
        ("write", Some(subargs)) => {
            let contents = subargs.value_of("contents").unwrap();
            let file = subargs.value_of("file").unwrap();
            if subargs.is_present("encoded") {
                decode_and_write_file(contents, file)
            } else {
                write_file(contents, file)
            }
            .map(|_| 0)
        }
        ("cat", Some(subargs)) => {
            let file = subargs.value_of("file").unwrap();
            if subargs.is_present("encoded") {
                read_and_encode_file(file)
            } else {
                read_file(file)
            }
            .map(|contents| {
                println!("{}", contents);
                0
            })
        }
        ("backup", Some(subcommand)) => run_backup(&docker, rt, subcommand),
        ("restore", Some(subcommand)) => run_restore(&docker, rt, subcommand),
        _ => print_usage(&args),
    };

    match result {
        Ok(i) => exit(i),
        Err(e) => {
            log::error!("Command failed: {:#}", e);
            exit(1)
        }
    };
}

fn print_usage(args: &ArgMatches) -> Result<i32> {
    println!("{}", args.usage());
    Ok(1)
}

fn run_restore(docker: &Docker, mut rt: Runtime, subcommand: &ArgMatches) -> Result<i32> {
    match subcommand.subcommand() {
        ("directory", Some(subargs)) => {
            let archive = subargs.value_of("ARCHIVE").unwrap();
            let output = subargs.value_of("OUTPUT").unwrap();
            restore_directory(archive, output).map(|_| 0)
        }
        ("volume", Some(subargs)) => {
            let archive = subargs.value_of("ARCHIVE").unwrap();
            let input = subargs.value_of("INPUT").unwrap();
            let volume = subargs.value_of("VOLUME").unwrap();
            let volume_mount = if subargs.value_of("volume_type").unwrap() == "directory" {
                get_bind_mount(volume.to_string())
            } else {
                get_volume_mount(volume.to_string())
            };
            let backup_mount = if subargs.value_of("input_type").unwrap() == "directory" {
                get_backup_directory_mount(input.to_string())
            } else {
                get_backup_volume_mount(input.to_string())
            };
            rt.block_on(restore_volume(
                &docker,
                archive.to_string(),
                backup_mount,
                volume_mount,
            ))
            .map(|_| 0)
        }
        ("container", Some(subargs)) => {
            let file = subargs.value_of("FILE").unwrap();
            let input = subargs.value_of("INPUT").unwrap();
            let name = subargs.value_of("NAME").unwrap();
            let backup_mount = if subargs.value_of("input_type").unwrap() == "directory" {
                get_backup_directory_mount(input.to_string())
            } else {
                get_backup_volume_mount(input.to_string())
            };
            rt.block_on(restore_container(&docker, file, name, backup_mount))
                .map(|_| 0)
        }
        _ => print_usage(subcommand),
    }
}

fn run_backup(docker: &Docker, mut rt: Runtime, subcommand: &ArgMatches) -> Result<i32> {
    match subcommand.subcommand() {
        ("directory", Some(subargs)) => {
            let archive_name = subargs.value_of("name").unwrap();
            let input = subargs.value_of("INPUT").unwrap();
            let output = subargs.value_of("OUTPUT").unwrap();
            backup_directory(archive_name, input, output).map(|p| {
                log::info!(
                    "Successfully backed up directory {} to {}",
                    input,
                    p.display()
                );
                0
            })
        }
        (subcommand, Some(subargs)) if subcommand == "container" || subcommand == "volume" => {
            let resource_name = subargs.value_of("NAME").unwrap();
            let output = subargs.value_of("OUTPUT").unwrap();
            let backup_mount = if subargs.value_of("output_type").unwrap() == "directory" {
                get_backup_directory_mount(output.to_string())
            } else {
                get_backup_volume_mount(output.to_string())
            };
            match subcommand {
                "volume" => rt
                    .block_on(backup_volume(
                        &docker,
                        resource_name.to_string(),
                        backup_mount,
                    ))
                    .map(|p| {
                        log::info!(
                            "Successfully backed up volume {} to {}",
                            resource_name,
                            p.display()
                        );
                        0
                    }),
                "container" => rt
                    .block_on(backup_container(
                        &docker,
                        resource_name,
                        backup_mount,
                        subargs.values_of_lossy("volumes"),
                    ))
                    .map(|p| {
                        log::info!(
                            "Successfully backed up container {} to {}",
                            resource_name,
                            p.display()
                        );
                        0
                    }),
                _ => print_usage(subargs),
            }
        }
        _ => print_usage(subcommand),
    }
}

fn init_docker() -> Result<(Runtime, Docker)> {
    let rt = Runtime::new()?;
    let docker = Docker::connect_with_unix_defaults()?;
    Ok((rt, docker))
}
