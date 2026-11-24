use std::time::Duration;

macro_rules! status {
    ($($arg:tt)*) => {
        crate::interaction::report(format_args!($($arg)*))
    };
}

#[cfg(windows)]
thread_local! {
    // Only the keepalive thread has a window connection. Unit tests use the console.
    static WINDOW: std::cell::OnceCell<crate::windows_worker::Worker> = const { std::cell::OnceCell::new() };
}

#[cfg(windows)]
pub(crate) fn connect_window(worker: crate::windows_worker::Worker) {
    WINDOW.with(|window| assert!(window.set(worker).is_ok()));
}

pub(crate) fn report(message: std::fmt::Arguments<'_>) {
    if !report_to_window(message) {
        println!("{message}");
    }
}

#[cfg(windows)]
pub(crate) fn report_error(message: std::fmt::Arguments<'_>) {
    if !report_to_window(message) {
        eprintln!("{message}");
    }
}

fn report_to_window(message: std::fmt::Arguments<'_>) -> bool {
    #[cfg(windows)]
    return WINDOW.with(|window| {
        window.get().is_some_and(|worker| {
            worker.report(message);
            true
        })
    });
    #[cfg(not(windows))]
    {
        let _ = message;
        false
    }
}

#[cfg(windows)]
pub(crate) fn window_credentials() -> Option<Option<crate::config::Credentials>> {
    WINDOW.with(|window| window.get().map(|worker| worker.credentials()))
}

pub(crate) fn stopping() -> bool {
    #[cfg(windows)]
    return WINDOW.with(|window| window.get().is_some_and(|worker| worker.stopping()));
    #[cfg(not(windows))]
    false
}

pub(crate) fn wait(duration: Duration) {
    #[cfg(windows)]
    if WINDOW.with(|window| window.get().is_some()) {
        if !stopping() {
            std::thread::park_timeout(duration);
        }
        return;
    }
    std::thread::sleep(duration);
}
