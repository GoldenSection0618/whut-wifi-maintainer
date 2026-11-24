use crate::config::Credentials;
use serde_json::json;
use std::fmt::Arguments;
use std::io::{self, BufRead, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, channel},
};
use std::thread;

pub(crate) struct Worker {
    input: Receiver<Credentials>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    pub(crate) fn report(&self, message: Arguments<'_>) {
        send(json!({ "kind": "status", "message": message.to_string() }));
    }

    pub(crate) fn credentials(&self) -> Option<Credentials> {
        if self.stopping() {
            return None;
        }
        send(json!({ "kind": "credentials_needed" }));
        self.input.recv().ok().filter(|_| !self.stopping())
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

fn send(update: serde_json::Value) {
    let mut output = io::stdout().lock();
    let _ = writeln!(output, "{update}").and_then(|()| output.flush());
}

pub(crate) fn connect() {
    let (sender, input) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let keepalive = thread::current();
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let Ok(Credentials { username, password }) = serde_json::from_str(&line) else {
                continue;
            };
            let username = username.trim().to_owned();
            if !username.is_empty()
                && !password.is_empty()
                && sender.send(Credentials { username, password }).is_err()
            {
                break;
            }
        }
        reader_stop.store(true, Ordering::Relaxed);
        drop(sender);
        keepalive.unpark();
    });
    crate::interaction::connect_window(Worker { input, stop });
}
