// SPDX-License-Identifier: GPL-3.0-only

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let _ = tracing_log::LogTracer::init();

    tracing::info!("Starting active window title applet with version {VERSION}");

    cosmic_applet_app_title::run()
}
