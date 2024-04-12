use std::path::{Path, PathBuf};
use std::{collections::HashMap, time::Duration};
use url2::Url2;

use tauri::api::process::{Command, CommandEvent};

use crate::errors::{LairKeystoreError, LaunchChildError};

pub async fn launch_lair_keystore_process(
    log_level: log::Level,
    keystore_data_dir: PathBuf,
    password: String,
) -> Result<Url2, LairKeystoreError> {
    let mut envs = HashMap::new();
    envs.insert(String::from("RUST_LOG"), String::from(log_level.as_str()));

    let mut keystore_path = keystore_data_dir.clone();

    // On Unix systems, there is a limit to the path length of a domain socket. Create a symlink to the lair directory from the tempdir
    // instead and overwrite the connectionUrl in the lair-keystore-config.yaml
    if cfg!(target_family = "unix") {
        let uid = nanoid::nanoid!(13);
        let src_path = std::env::temp_dir().join(format!("lair.{}", uid));
        symlink::symlink_dir(keystore_path, src_path.clone())
            .map_err(|e| LairKeystoreError::ErrorCreatingSymLink(e.to_string()))?;
        keystore_path = src_path;

        // overwrite connectionUrl in lair-keystore-config.yaml to symlink directory
        // 1. read to string
        let mut lair_config_string =
            std::fs::read_to_string(keystore_path.join("lair-keystore-config.yaml"))
                .map_err(|e| LairKeystoreError::ErrorReadingLairConfig(e.to_string()))?;

        // 2. filter out the line with the connectionUrl
        let connection_url_line = lair_config_string
            .lines()
            .filter(|line| line.contains("connectionUrl:"))
            .collect::<String>();

        // 3. replace the part unix:///home/[user]/.local/share/holochain-launcher/profiles/default/lair/0.2/socket?k=[some_key]
        //    with unix://[path to tempdir]/socket?k=[some_key]
        let split_byte_index = connection_url_line.rfind("socket?").unwrap();
        let socket = &connection_url_line.as_str()[split_byte_index..];
        let tempdir_connection_url = match url::Url::parse(&format!(
            "unix://{}",
            keystore_path.join(socket).to_str().unwrap(),
        )) {
            Ok(url) => url,
            Err(e) => {
                return Err(LairKeystoreError::OtherError(format!(
                    "Failed to parse URL for symlink lair path: {}",
                    e
                )))
            }
        };

        let new_line = &format!("connectionUrl: {}\n", tempdir_connection_url);

        // 4. Replace the existing connectionUrl line with that new line
        lair_config_string = LinesWithEndings::from(lair_config_string.as_str())
            .map(|line| {
                if line.contains("connectionUrl:") {
                    new_line
                } else {
                    line
                }
            })
            .collect::<String>();

        // 5. Overwrite the lair-keystore-config.yaml with the modified content
        std::fs::write(
            keystore_data_dir.join("lair-keystore-config.yaml"),
            lair_config_string,
        )
        .map_err(|e| LairKeystoreError::ErrorWritingLairConfig(e.to_string()))?;
    }

    // NEW_VERSION Check whether lair-keystore version needs to get updated
    let (mut lair_rx, mut command_child) = Command::new_sidecar("lair-keystore-v0.4.2")
        .or(Err(LairKeystoreError::LaunchChildError(
            LaunchChildError::BinaryNotFound,
        )))?
        .args(&["server", "-p"])
        .current_dir(keystore_path.clone())
        .envs(envs.clone())
        .spawn()
        .map_err(|err| {
            LairKeystoreError::LaunchChildError(LaunchChildError::FailedToExecute(format!(
                "{:?}",
                err
            )))
        })?;

    tauri::async_runtime::spawn(async move {
        std::thread::sleep(Duration::from_millis(10));
        command_child
            .write(password.as_bytes())
            .expect("Could not write password");
    });

    let mut started = false;
    while !started {
        if let Some(event) = lair_rx.recv().await {
            match event.clone() {
                CommandEvent::Stdout(line) => {
                    log::info!("[LAIR] {}", line);
                    if line.contains("lair-keystore running") {
                        started = true;
                    }
                }
                CommandEvent::Stderr(line) => {
                    log::error!("[LAIR] {}", line);
                    if line.contains("InternalSodium") {
                        return Err(LairKeystoreError::IncorrectPassword);
                    }
                }
                _ => {
                    log::info!("[LAIR] {:?}", event);
                }
            }
        }
    }

    tauri::async_runtime::spawn(async move {
        // read events such as stdout
        while let Some(event) = lair_rx.recv().await {
            match event.clone() {
                CommandEvent::Stdout(line) => log::info!("[LAIR] {}", line),
                CommandEvent::Stderr(line) => log::error!("[LAIR] {}", line),
                _ => log::info!("[LAIR] {:?}", event),
            }
        }
    });

    // NEW_VERSION Check whether lair-keystore version needs to get updated
    let output = Command::new_sidecar("lair-keystore-v0.4.2")
        .or(Err(LairKeystoreError::LaunchChildError(
            LaunchChildError::BinaryNotFound,
        )))?
        .args(&["url"])
        .current_dir(keystore_path)
        .envs(envs.clone())
        .output()
        .map_err(|err| {
            LairKeystoreError::LaunchChildError(LaunchChildError::FailedToExecute(format!(
                "{:?}",
                err
            )))
        })?;

    if output.stderr.len() > 0 {
        return Err(LairKeystoreError::LaunchChildError(
            LaunchChildError::FailedToExecute(output.stderr),
        ));
    }

    let url = Url2::parse(output.stdout);

    log::info!("Launched lair-keystore");

    Ok(url)
}

pub async fn initialize_keystore(
    keystore_dir: PathBuf,
    password: String,
) -> Result<(), LairKeystoreError> {
    // NEW_VERSION Check whether lair-keystore version needs to get updated
    let (mut lair_rx, mut command_child) = Command::new_sidecar("lair-keystore-v0.4.2")
        .or(Err(LairKeystoreError::LaunchChildError(
            LaunchChildError::BinaryNotFound,
        )))?
        .args(&["init", "-p"])
        .current_dir(keystore_dir)
        .spawn()
        .map_err(|err| LaunchChildError::FailedToExecute(format!("{:?}", err)))?;

    tauri::async_runtime::spawn(async move {
        std::thread::sleep(Duration::from_millis(10));
        command_child
            .write(password.as_bytes())
            .expect("Could not write password");
    });

    let mut started = false;
    while !started {
        if let Some(event) = lair_rx.recv().await {
            match event.clone() {
                CommandEvent::Stdout(line) => {
                    log::info!("[LAIR] {}", line);
                    if line.contains("lair-keystore init connection_url") {
                        started = true;
                    }
                }
                CommandEvent::Stderr(line) => {
                    log::error!("[LAIR] {}", line);
                    if line.contains("InternalSodium") {
                        return Err(LairKeystoreError::IncorrectPassword);
                    }
                }
                _ => {
                    log::info!("[LAIR] {:?}", event);
                }
            }
        }
    }

    Ok(())
}

/// Iterator yielding every line in a string. The line includes newline character(s).
/// https://stackoverflow.com/questions/40455997/iterate-over-lines-in-a-string-including-the-newline-characters
pub struct LinesWithEndings<'a> {
    input: &'a str,
}

impl<'a> LinesWithEndings<'a> {
    pub fn from(input: &'a str) -> LinesWithEndings<'a> {
        LinesWithEndings { input: input }
    }
}

impl<'a> Iterator for LinesWithEndings<'a> {
    type Item = &'a str;

    #[inline]
    fn next(&mut self) -> Option<&'a str> {
        if self.input.is_empty() {
            return None;
        }
        let split = self
            .input
            .find('\n')
            .map(|i| i + 1)
            .unwrap_or(self.input.len());
        let (line, rest) = self.input.split_at(split);
        self.input = rest;
        Some(line)
    }
}
