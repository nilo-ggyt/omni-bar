//! Observe readiness and release through actual AppKit events on a dedicated main thread.
//! These observations measure neither physical keyboard input nor presented frames.

/// Report ordinary tool failure as exit=1 rather than disguising environment/resource errors as aborts.
fn main() -> Result<(), String> {
    // Explicit example execution keeps real window acquisition out of ordinary unit-test discovery.
    let samples = std::env::args()
        .nth(1)
        .map(|value| {
            // Invalid numeric configuration fails rather than silently choosing a different workload size.
            value
                .parse()
                .map_err(|_| "Launcher probe sample count must be numeric".to_owned())
        })
        .transpose()?
        .unwrap_or(1000);
    let mtm = objc2_foundation::MainThreadMarker::new().ok_or("native main thread required")?;
    // The parent's watchdog bounds missing events; timers, sleeps and polling never establish readiness or release.
    let result = omni_native_runtime::run_native_launcher_runtime_probe(samples, mtm)?;
    println!("{result:#?}");
    Ok(())
}
