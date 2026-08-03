use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::append::rolling_file::RollingFileAppender;
use log4rs::append::rolling_file::policy::compound::CompoundPolicy;
use log4rs::append::rolling_file::policy::compound::roll::fixed_window::FixedWindowRoller;
use log4rs::append::rolling_file::policy::compound::trigger::size::SizeTrigger;
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;
use log4rs::filter::threshold::ThresholdFilter;
use sentry::integrations::log::SentryLogger;

const FILE_APPENDER_NAME: &str = "file";
const CONSOLE_APPENDER_NAME: &str = "console";

const LOG_FILE_PATH: &str = "app.log";

/// Installs the global logger.
///
/// When `forward_to_sentry` is set, log4rs is wrapped in a `SentryLogger`: `error!`
/// records are captured as Sentry events, `warn!`/`info!` become breadcrumbs and
/// `debug!`/`trace!` are only written to the local appender. This is how errors from
/// route handlers and background tasks reach Sentry — no explicit capture calls.
pub fn init(logging_level: &str, log_target: &str, forward_to_sentry: bool) -> anyhow::Result<()> {
    let config = get_logging_config(logging_level, log_target);

    if !forward_to_sentry {
        log4rs::init_config(config)?;
        return Ok(());
    }

    let level = get_logging_level_from_string(logging_level);

    log::set_boxed_logger(Box::new(sentry_logger(config)))?;
    log::set_max_level(level);

    Ok(())
}

fn sentry_logger(config: Config) -> SentryLogger<log4rs::Logger> {
    SentryLogger::with_dest(log4rs::Logger::new(config))
}

pub fn get_logging_config(logging_level: &str, log_target: &str) -> Config {
    let level = get_logging_level_from_string(logging_level);

    match log_target {
        "file" => Config::builder()
            .appender(get_rolling_appender(level))
            .logger(get_default_logger(level))
            .build(Root::builder().appender(FILE_APPENDER_NAME).build(level))
            .unwrap_or_else(|_| panic!("unable to create log file '{}'", LOG_FILE_PATH)),
        _ => Config::builder()
            .appender(get_console_appender(level))
            .logger(get_default_logger(level))
            .build(Root::builder().appender(CONSOLE_APPENDER_NAME).build(level))
            .expect("unable to create console logging configuration"),
    }
}

fn get_logging_level_from_string(level: &str) -> LevelFilter {
    match level {
        "debug" => LevelFilter::Debug,
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "trace" => LevelFilter::Trace,
        "off" => LevelFilter::Off,
        _ => LevelFilter::Info,
    }
}

fn get_rolling_appender(level: LevelFilter) -> Appender {
    let log_file_format = format!("{}.{{}}", LOG_FILE_PATH);

    let fixed_window_roller = FixedWindowRoller::builder()
        .build(&log_file_format, 5)
        .expect("couldn't build fixed window roller");

    let size_trigger = SizeTrigger::new(100_000_000);
    let policy = CompoundPolicy::new(Box::new(size_trigger), Box::new(fixed_window_roller));
    let rolling_appender = RollingFileAppender::builder()
        .encoder(get_encoder())
        .build(LOG_FILE_PATH, Box::new(policy))
        .expect("couldn't build rolling appender");

    Appender::builder()
        .filter(Box::new(ThresholdFilter::new(level)))
        .build(FILE_APPENDER_NAME, Box::new(rolling_appender))
}

fn get_encoder() -> Box<PatternEncoder> {
    Box::new(PatternEncoder::new(
        "{d(%Y-%m-%d %H:%M:%S)} - {l} - [{M}] - {m}{n}",
    ))
}

fn get_console_appender(level: LevelFilter) -> Appender {
    let console_appender = ConsoleAppender::builder().encoder(get_encoder()).build();

    Appender::builder()
        .filter(Box::new(ThresholdFilter::new(level)))
        .build(CONSOLE_APPENDER_NAME, Box::new(console_appender))
}

fn get_default_logger(level: LevelFilter) -> Logger {
    Logger::builder().build("default", level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log;
    use sentry::test::with_captured_events;

    fn log_at(level: log::Level, message: &str) -> Vec<sentry::protocol::Event<'static>> {
        let logger = sentry_logger(get_logging_config("info", "stdout"));

        with_captured_events(|| {
            logger.log(
                &log::Record::builder()
                    .level(level)
                    .target("kwp::route::receive_webhook")
                    .args(format_args!("{}", message))
                    .build(),
            );
        })
    }

    #[test]
    fn error_records_become_sentry_events() {
        let events = log_at(log::Level::Error, "failed to store webhook");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, sentry::Level::Error);
        assert_eq!(
            events[0].logger.as_deref(),
            Some("kwp::route::receive_webhook")
        );
    }

    #[test]
    fn warn_and_info_records_do_not_become_events() {
        assert!(log_at(log::Level::Warn, "invalid webhook secret").is_empty());
        assert!(log_at(log::Level::Info, ">>> incoming webhook").is_empty());
    }

    #[test]
    fn debug_records_stay_local() {
        assert!(log_at(log::Level::Debug, "verifying webhook secret").is_empty());
    }

    #[test]
    fn breadcrumbs_from_warnings_are_attached_to_the_next_event() {
        let logger = sentry_logger(get_logging_config("info", "stdout"));

        let events = with_captured_events(|| {
            logger.log(
                &log::Record::builder()
                    .level(log::Level::Warn)
                    .target("kwp::route::receive_webhook")
                    .args(format_args!("invalid webhook secret for channel: telegram"))
                    .build(),
            );
            logger.log(
                &log::Record::builder()
                    .level(log::Level::Error)
                    .target("kwp::route::receive_webhook")
                    .args(format_args!("failed to store webhook"))
                    .build(),
            );
        });

        assert_eq!(events.len(), 1);
        let breadcrumbs = &events[0].breadcrumbs;
        assert_eq!(breadcrumbs.len(), 1);
        assert_eq!(
            breadcrumbs[0].message.as_deref(),
            Some("invalid webhook secret for channel: telegram")
        );
        assert_eq!(breadcrumbs[0].level, sentry::Level::Warning);
    }
}
