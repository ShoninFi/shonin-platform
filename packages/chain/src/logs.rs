//! What every service writes to its log.
//!
//! A paid RPC URL carries its key, and the HTTP stack under the chain client
//! names the URL it is sending to in what it logs below `info`: alloy's HTTP
//! transport opens a `debug` span carrying the whole URL around every
//! request, and the HTTP client and its connection pool trace the bytes and
//! hosts they handle. So whatever `RUST_LOG` asks for, those crates are held
//! at `info` or quieter, and turning `debug` on to look into a vault never
//! writes the key to the log. A directive scoped to a span would get round
//! that (it enables everything inside the spans it names, the transport's
//! included), so those are dropped: an operator narrows the log by target.

use std::str::FromStr;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

/// The crates that name a request's URL, or its bytes, below `info`.
const NAMES_THE_URL: [&str; 4] = ["alloy_transport_http", "reqwest", "hyper", "hyper_util"];

/// The filter every service logs through: `RUST_LOG`, `info` when it is not
/// set, with the HTTP stack held at `info` at most.
pub fn log_filter() -> EnvFilter {
    log_filter_from(&std::env::var("RUST_LOG").unwrap_or_default())
}

/// The same, from a spec written as `RUST_LOG` would be.
///
/// A directive scoped to a span or a field (`target[span{field}]=level`) is
/// dropped, whatever its target: it enables every span and event inside the
/// spans it names, whatever their own target, so one naming a span the
/// service makes requests in would open the transport's span within it. A
/// directive whose target is one of the HTTP crates, lies inside one, or is
/// the start of one's name (`alloy`, `hyper`) is dropped too, except one
/// naming an HTTP crate exactly at a level quieter than `info`, which is
/// kept. A malformed directive is dropped, the way `EnvFilter` itself would
/// ignore it.
pub fn log_filter_from(spec: &str) -> EnvFilter {
    let mut global = LevelFilter::INFO;
    let mut kept = Vec::new();
    let mut quieter: Vec<(&str, LevelFilter)> = Vec::new();
    for directive in spec.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        // A bare level is the default for everything.
        if let Ok(level) = LevelFilter::from_str(directive) {
            global = level;
            kept.push(directive.to_string());
            continue;
        }
        // `target=level`; with no level, everything.
        let (target, level) = match directive.rsplit_once('=') {
            Some((target, level)) => match LevelFilter::from_str(level) {
                Ok(level) => (target, level),
                Err(_) => (directive, LevelFilter::TRACE),
            },
            None => (directive, LevelFilter::TRACE),
        };
        if directive.contains('[') || directive.contains('{') {
            continue;
        }
        if let Some(pinned) = NAMES_THE_URL.iter().find(|pinned| target == **pinned)
            && level < LevelFilter::INFO
        {
            quieter.push((pinned, level));
            continue;
        }
        if NAMES_THE_URL
            .iter()
            .any(|pinned| target.starts_with(pinned) || pinned.starts_with(target))
        {
            continue;
        }
        kept.push(directive.to_string());
    }
    let mut filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .parse_lossy(kept.join(","));
    for pinned in NAMES_THE_URL {
        let level = quieter
            .iter()
            .find(|(target, _)| *target == pinned)
            .map(|(_, level)| *level)
            .unwrap_or(LevelFilter::INFO)
            .min(global);
        if let Ok(directive) = format!("{pinned}={level}").parse() {
            filter = filter.add_directive(directive);
        }
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::*;

    fn says(spec: &str) -> String {
        log_filter_from(spec).to_string()
    }

    #[test]
    fn the_http_stack_is_held_at_info_whatever_is_asked() {
        for spec in [
            "",
            "debug",
            "trace",
            "alloy_transport_http=trace",
            "alloy_transport_http::reqwest_transport=debug,reqwest=trace",
            "hyper_util::client=trace,executor=debug",
            "[ReqwestTransport]=debug",
            "[{url}]=trace",
            "alloy[ReqwestTransport]=debug",
            "keeper[round]=trace",
            "alloy=trace",
            "hyper=debug",
        ] {
            let filter = says(spec);
            for pinned in NAMES_THE_URL {
                assert!(
                    filter.contains(&format!("{pinned}=info")),
                    "{spec}: {filter}"
                );
            }
            assert!(!filter.contains('['), "{spec}: {filter}");
            for verbose in ["debug", "trace"] {
                for pinned in NAMES_THE_URL {
                    assert!(
                        !filter.contains(&format!("{pinned}={verbose}")),
                        "{spec}: {filter}"
                    );
                }
            }
        }
    }

    #[test]
    fn what_else_is_asked_for_is_kept_and_span_directives_are_not() {
        let filter = says("executor=debug,keeper[round]=trace,chain::client=trace");
        assert!(filter.contains("executor=debug"), "{filter}");
        assert!(filter.contains("chain::client=trace"), "{filter}");
        assert!(!filter.contains("keeper"), "{filter}");
    }

    #[test]
    fn a_target_that_is_the_start_of_an_http_crate_is_dropped() {
        for spec in [
            "alloy=debug",
            "alloy_transport=trace",
            "a=trace",
            "hyp=debug",
        ] {
            let filter = says(spec);
            let target = spec.split('=').next().unwrap();
            assert!(
                !filter
                    .split(',')
                    .any(|directive| directive.starts_with(&format!("{target}="))),
                "{spec}: {filter}"
            );
        }
    }

    #[test]
    fn a_quieter_level_for_the_http_stack_is_kept() {
        let filter = says("reqwest=warn");
        assert!(filter.contains("reqwest=warn"), "{filter}");
        let filter = says("error");
        assert!(filter.contains("hyper=error"), "{filter}");
        // Never louder than info, even when everything else is.
        let filter = says("reqwest=debug,trace");
        assert!(filter.contains("reqwest=info"), "{filter}");
    }
}
