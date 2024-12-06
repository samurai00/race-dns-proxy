use time::macros::{format_description, offset};
use tracing_subscriber::{
    filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt, Layer as _,
};

pub fn init_logger(
    log_level_filter: &str,
    error_log: Option<String>,
) -> Vec<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| log_level_filter.into());

    // Create custom time format: yyyy-MM-dd HH:mm:ss.SSS
    let format =
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]");
    let timer = tracing_subscriber::fmt::time::OffsetTime::new(offset!(+8), format);

    let mut guards = vec![];

    let mut stdout = None;
    let non_blocking;
    let guard;

    if let Some(error_log) = error_log {
        let base_path = std::path::Path::new(&error_log).parent().unwrap();
        let filename = std::path::Path::new(&error_log).file_name().unwrap();
        let file_appender = tracing_appender::rolling::daily(base_path, filename);
        (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let (stdout_non_blocking, guard_stdout) = tracing_appender::non_blocking(std::io::stdout());
        guards.push(guard_stdout);
        stdout = Some(stdout_non_blocking)
    } else {
        (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());
    }
    guards.push(guard);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_timer(timer.clone())
                .with_writer(non_blocking.clone()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_timer(timer.clone())
                .with_writer(stdout.clone().unwrap_or(non_blocking))
                .with_filter(filter_fn(move |metadata| {
                    stdout.is_some()
                        && (metadata.target().ends_with(":stdout")
                            || metadata.target().ends_with(":stderr"))
                })),
        )
        .init();

    guards
}
