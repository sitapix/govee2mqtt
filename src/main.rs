use clap::Parser;

fn setup_logger() {
    fn resolve_timezone() -> chrono_tz::Tz {
        std::env::var("TZ")
            .or_else(|_| iana_time_zone::get_timezone())
            .ok()
            .and_then(|name| name.parse().ok())
            .unwrap_or(chrono_tz::UTC)
    }

    let tz = resolve_timezone();
    let utc_suffix = if tz == chrono_tz::UTC { "Z" } else { "" };

    env_logger::builder()
        .format(move |buf, record| {
            use chrono::Utc;
            use std::io::Write;

            let level_style = buf.default_level_style(record.level());
            write!(
                buf,
                "[{}{utc_suffix} ",
                Utc::now().with_timezone(&tz).format("%Y-%m-%dT%H:%M:%S")
            )?;
            write!(buf, "{level_style}{:<5}{level_style:#}", record.level())?;
            if let Some(path) = record.module_path() {
                write!(buf, " {}", path)?;
            }
            writeln!(buf, "] {}", record.args())?;

            // Capture into the log streaming system
            let level_str = format!("{}", record.level());
            let target = record.module_path().unwrap_or("").to_string();
            let message = format!("{}", record.args());
            govee::service::log_capture::push_log(&level_str, &target, &message);

            // Write to rotating log file
            let file_line = format!(
                "[{}{utc_suffix} {:<5} {}] {}",
                chrono::Utc::now()
                    .with_timezone(&tz)
                    .format("%Y-%m-%dT%H:%M:%S"),
                record.level(),
                target,
                message
            );
            govee::service::file_logger::write_line(&file_line);

            Ok(())
        })
        .filter_level(log::LevelFilter::Info)
        .parse_env("RUST_LOG")
        .init();
}

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    color_backtrace::install();
    if let Ok(path) = dotenvy::dotenv() {
        eprintln!("Loading environment overrides from {path:?}");
    }

    govee::service::file_logger::init();
    setup_logger();

    let args = govee::Args::parse();
    args.run().await
}
