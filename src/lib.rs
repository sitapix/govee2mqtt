pub mod ble;
pub mod cache;
pub mod commands;
pub mod hass_mqtt;
pub mod lan_api;
pub mod music;
#[macro_use]
pub mod platform_api;
pub mod rest_api;
pub mod service;
pub mod temperature;
pub mod undoc_api;
pub mod version_info;

pub use undoc_api::UndocApiArguments;

#[derive(clap::Parser, Debug)]
#[command(version = version_info::govee_version(), propagate_version=true)]
pub struct Args {
    #[command(flatten)]
    pub api_args: platform_api::GoveeApiArguments,
    #[command(flatten)]
    pub lan_disco_args: lan_api::LanDiscoArguments,
    #[command(flatten)]
    pub undoc_args: UndocApiArguments,
    #[command(flatten)]
    pub hass_args: service::hass::HassArguments,

    #[command(subcommand)]
    pub cmd: SubCommand,
}

#[derive(clap::Parser, Debug)]
pub enum SubCommand {
    LanControl(commands::lan_control::LanControlCommand),
    LanDisco(commands::lan_disco::LanDiscoCommand),
    ListHttp(commands::list_http::ListHttpCommand),
    List(commands::list::ListCommand),
    HttpControl(commands::http_control::HttpControlCommand),
    Serve(commands::serve::ServeCommand),
    Undoc(commands::undoc::UndocCommand),
}

impl Args {
    pub async fn run(&self) -> anyhow::Result<()> {
        match &self.cmd {
            SubCommand::LanControl(cmd) => cmd.run(self).await,
            SubCommand::LanDisco(cmd) => cmd.run(self).await,
            SubCommand::ListHttp(cmd) => cmd.run(self).await,
            SubCommand::HttpControl(cmd) => cmd.run(self).await,
            SubCommand::List(cmd) => cmd.run(self).await,
            SubCommand::Serve(cmd) => cmd.run(self).await,
            SubCommand::Undoc(cmd) => cmd.run(self).await,
        }
    }
}

pub fn opt_env_var<T: std::str::FromStr>(name: &str) -> anyhow::Result<Option<T>>
where
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    use anyhow::Context;

    let log_sensitive_data =
        !name.contains("PASSWORD") || undoc_api::should_log_sensitive_data();

    match std::env::var(name) {
        Ok(p) => Ok(Some(p.parse().map_err(|err| {
            let mut message = format!("{err:#}");
            if !log_sensitive_data {
                message = message.replace(&p, "REDACTED");
            }
            anyhow::anyhow!("parsing ${name}: {message}")
        })?)),
        Err(std::env::VarError::NotPresent) => {
            let secret_env_name = format!("{}_FILE", name);

            match std::env::var(&secret_env_name) {
                Ok(path) => {
                    let content = std::fs::read_to_string(&path).with_context(|| {
                        format!(
                            "Reading secret for {name} from path defined in {secret_env_name}: {path}"
                        )
                    })?;

                    let trimmed_content = content.trim_end();

                    Ok(Some(trimmed_content.parse().map_err(|err| {
                        let mut message = format!("{err:#}");
                        if !log_sensitive_data {
                            message = message.replace(trimmed_content, "REDACTED");
                        }
                        anyhow::anyhow!("parsing secret content for {name}: {message}")
                    })?))
                }
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(err) => anyhow::bail!("${secret_env_name} is invalid: {err:#}"),
            }
        }
        Err(err) => anyhow::bail!("${name} is invalid: {err:#}"),
    }
}
