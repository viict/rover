use std::env::var;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

use anyhow::{anyhow, Context};
use crossbeam_channel::Sender;
use rover_client::operations::config::who_am_i::{self, Actor, ConfigWhoAmIInput};
use rover_std::Emoji;

use crate::options::ProfileOpt;
use crate::utils::client::StudioClientConfig;
use crate::{command::dev::do_dev::log_err_and_continue, RoverError, RoverResult};

#[derive(Debug)]
pub struct BackgroundTask {
    child: Child,
    descriptor: String,
}

pub enum BackgroundTaskLog {
    Stdout(String),
    Stderr(String),
}

impl BackgroundTask {
    pub fn new(
        command: String,
        log_sender: Sender<BackgroundTaskLog>,
        client_config: &StudioClientConfig,
        profile_opt: &ProfileOpt,
    ) -> RoverResult<Self> {
        let descriptor = command.clone();
        let args: Vec<&str> = command.split(' ').collect();
        let (bin, args) = match args.len() {
            0 => Err(anyhow!("the command you passed is empty")),
            1 => Ok((args[0], Vec::new())),
            _ => Ok((args[0], Vec::from_iter(args[1..].iter()))),
        }?;
        tracing::info!("starting `{}`", &command);

        if which::which(bin).is_err() {
            return Err(anyhow!("{} is not installed on this machine", &bin).into());
        }

        let mut command = Command::new(bin);
        command.args(args).env("APOLLO_ROVER", "true");

        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        if let Ok(apollo_graph_ref) = var("APOLLO_GRAPH_REF") {
            command.env("APOLLO_GRAPH_REF", apollo_graph_ref);
            if let Some(api_key) = client_config.get_authenticated_client(profile_opt).map_err(|err| {
                eprintln!("{} APOLLO_GRAPH_REF is set, but credentials could not be loaded. \
                Enterprise features within the router will not function. {err}", Emoji::Warn);
            }).ok().and_then(|client| {
                who_am_i::run(ConfigWhoAmIInput {}, &client).map_or_else(|err| {
                    eprintln!("{} Could not determine the type of configured credentials, \
                    Router may fail to start if Enterprise features are enabled. {err}", Emoji::Warn);
                    Some(client.credential.api_key.clone())
                }, |identity| {
                    match identity.key_actor_type {
                        Actor::GRAPH => Some(client.credential.api_key.clone()),
                        _ => {
                            eprintln!(
                                "{} APOLLO_GRAPH_REF is set, but the key provided is not a graph key. \
                                Enterprise features within the router will not function. \
                                Either select a `--profile` that is configured with a graph-specific \
                                key, or provide one via the APOLLO_KEY environment variable.", Emoji::Warn
                            );
                            eprintln!("{} you can configure a graph key by following the instructions at https://www.apollographql.com/docs/graphos/api-keys/#graph-api-keys", Emoji::Note);
                            None
                        }
                    }
                })
            }) { command.env("APOLLO_KEY", api_key); }
        }

        let mut child = command
            .spawn()
            .with_context(|| "could not spawn child process")?;

        if let Some(stdout) = child.stdout.take() {
            let log_sender = log_sender.clone();
            std::thread::spawn(move || {
                let stdout = BufReader::new(stdout);
                stdout.lines().for_each(|line| {
                    if let Ok(line) = line {
                        log_sender
                            .send(BackgroundTaskLog::Stdout(line))
                            .expect("could not update stdout logs for command");
                    }
                });
            });
        }

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let stderr = BufReader::new(stderr);
                stderr.lines().for_each(|line| {
                    if let Ok(line) = line {
                        log_sender
                            .send(BackgroundTaskLog::Stderr(line))
                            .expect("could not update stderr logs for command");
                    }
                });
            });
        }

        Ok(Self { child, descriptor })
    }

    pub fn kill(&mut self) {
        let pid = self.id();
        tracing::info!("killing child with pid {}", &pid);
        let _ = self.child.kill().map_err(|_| {
            log_err_and_continue(RoverError::new(anyhow!(
                "could not kill child with pid {}",
                &pid
            )));
        });
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn descriptor(&self) -> &str {
        &self.descriptor
    }
}

impl Drop for BackgroundTask {
    fn drop(&mut self) {
        self.kill()
    }
}
