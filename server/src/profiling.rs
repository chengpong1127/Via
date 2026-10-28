use std::{error::Error, fs::File};
use tracing_chrome::{ChromeLayerBuilder, EventOrSpan, FlushGuard, TraceStyle};
use tracing_subscriber::{
    EnvFilter, Layer,
    filter::{FilterExt, filter_fn},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

pub fn init(log_filter: Option<EnvFilter>) -> Result<Option<FlushGuard>, Box<dyn Error>> {
    let (chrome, guard) = match std::env::var_os("VIA_CHROME_TRACE") {
        Some(path) => {
            let file = File::create(&path).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("cannot create VIA_CHROME_TRACE output: {error}"),
                )
            })?;
            let (layer, guard) = ChromeLayerBuilder::new()
                .writer(file)
                .trace_style(TraceStyle::Async)
                .name_fn(Box::new(|item| match item {
                    EventOrSpan::Span(span) => {
                        format!("{} #{}", span.metadata().name(), span.id().into_u64())
                    }
                    EventOrSpan::Event(event) => event.metadata().name().to_owned(),
                }))
                .include_args(true)
                .build();
            (
                Some(layer.with_filter(filter_fn(|metadata| {
                    metadata.target() == "via_profile" && metadata.is_span()
                }))),
                Some(guard),
            )
        }
        None => (None, None),
    };
    let logs = log_filter.map(|filter| {
        tracing_subscriber::fmt::layer()
            .without_time()
            .with_filter(filter.and(filter_fn(|metadata| metadata.target() != "via_profile")))
    });
    tracing_subscriber::registry()
        .with(logs)
        .with(chrome)
        .try_init()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(guard)
}
