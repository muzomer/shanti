//! The single stream of things the UI reacts to.
//!
//! Terminal input, the clock and (later) background work all reach the main loop
//! through one channel, so the loop never blocks on any one of them. That is what
//! lets the UI redraw while nothing is typed.

use std::{
    sync::{
        mpsc::{self, Receiver, RecvError, Sender},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use tracing::debug;

/// How often a tick is emitted, and therefore how often the UI redraws with no
/// input: 10 frames per second. Fast enough for a spinner to look alive, slow
/// enough that an idle shanti costs nothing measurable.
const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the input thread waits for a terminal event before checking whether
/// it has been asked to stop. It bounds shutdown latency only — real input is
/// still delivered the instant it arrives.
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Everything the main loop can be woken by.
#[derive(Debug)]
pub enum AppEvent {
    /// A key press. Key *releases* are dropped by the producer, so a component
    /// never sees the same keystroke twice on terminals that report both.
    Key(KeyEvent),
    /// A bracketed paste, delivered whole. Without this a pasted PR URL would
    /// arrive as one key event per character.
    Paste(String),
    /// The periodic clock. Nothing depends on it yet beyond the redraw.
    Tick,
    /// A background job finished.
    ///
    /// PLACEHOLDER: no worker sends this yet — shanti-hml.2 ("Add a background
    /// worker for slow operations") defines the real payload and the workers
    /// that produce it. The variant exists now so the loop already has the shape
    /// that issue needs.
    Job(JobResult),
}

/// PLACEHOLDER payload for [`AppEvent::Job`], replaced by shanti-hml.2.
#[derive(Debug)]
pub struct JobResult {
    /// Name of the job that finished, for logging until there is real content.
    pub name: String,
}

/// Producer threads plus the receiving end of their shared channel.
///
/// Dropping this asks both threads to stop and waits for them, so no thread can
/// outlive the session and keep the process alive after the user quits.
pub struct EventSource {
    receiver: Receiver<AppEvent>,
    sender: Sender<AppEvent>,
    shutdown: Arc<Shutdown>,
    threads: Vec<JoinHandle<()>>,
}

impl EventSource {
    /// Spawns the input reader and the ticker.
    pub fn new() -> Self {
        Self::with_tick_interval(TICK_INTERVAL)
    }

    pub fn with_tick_interval(tick: Duration) -> Self {
        let (sender, receiver) = mpsc::channel();
        let shutdown = Arc::new(Shutdown::default());

        let threads = vec![
            spawn_input_reader(sender.clone(), Arc::clone(&shutdown)),
            spawn_ticker(sender.clone(), Arc::clone(&shutdown), tick),
        ];

        Self {
            receiver,
            sender,
            shutdown,
            threads,
        }
    }

    /// Blocks until the next event. Fails only once every producer is gone,
    /// which cannot happen while this `EventSource` is alive.
    pub fn next(&self) -> Result<AppEvent, RecvError> {
        self.receiver.recv()
    }

    /// A handle background workers will use to report results.
    ///
    /// PLACEHOLDER, see [`AppEvent::Job`]: nothing calls this until shanti-hml.2.
    pub fn job_sender(&self) -> Sender<AppEvent> {
        self.sender.clone()
    }
}

impl Default for EventSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EventSource {
    fn drop(&mut self) {
        self.shutdown.request();
        for thread in self.threads.drain(..) {
            // A producer only fails to finish if it panicked; the session is
            // ending either way, so log it rather than panicking in turn.
            if thread.join().is_err() {
                debug!("an event producer thread panicked");
            }
        }
    }
}

/// A stop flag the ticker can also *wait* on, so quitting is not delayed by a
/// thread sleeping out the rest of its interval.
#[derive(Default)]
struct Shutdown {
    requested: Mutex<bool>,
    changed: Condvar,
}

impl Shutdown {
    fn request(&self) {
        if let Ok(mut requested) = self.requested.lock() {
            *requested = true;
        }
        self.changed.notify_all();
    }

    fn is_requested(&self) -> bool {
        self.requested.lock().is_ok_and(|requested| *requested)
    }

    /// Waits up to `timeout`, returning early as soon as a stop is requested.
    /// `true` means "stop now".
    ///
    /// The flag is tested *while holding the lock, before waiting*: a request
    /// that lands between two waits notifies nobody, and a waiter that ignored
    /// it would sleep out the whole interval before noticing.
    fn wait_timeout(&self, timeout: Duration) -> bool {
        let Ok(requested) = self.requested.lock() else {
            return true;
        };
        match self
            .changed
            .wait_timeout_while(requested, timeout, |requested| !*requested)
        {
            Ok((requested, _)) => *requested,
            Err(_) => true,
        }
    }
}

/// Reads terminal events and forwards the ones the app understands.
///
/// It polls instead of blocking in `event::read` so it always comes back to the
/// stop flag: a thread parked in `read` would sit there until the user pressed
/// one more key, keeping the process alive after quit.
fn spawn_input_reader(sender: Sender<AppEvent>, shutdown: Arc<Shutdown>) -> JoinHandle<()> {
    thread::spawn(move || {
        while !shutdown.is_requested() {
            match event::poll(INPUT_POLL_INTERVAL) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(error) => {
                    debug!(%error, "failed to poll terminal events; stopping the reader");
                    return;
                }
            }

            let event = match event::read() {
                Ok(event) => event,
                Err(error) => {
                    debug!(%error, "failed to read a terminal event; stopping the reader");
                    return;
                }
            };

            let app_event = match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => AppEvent::Key(key),
                Event::Paste(text) => AppEvent::Paste(text),
                // Resize redraws by itself on the next event; mouse and focus are
                // not bound to anything.
                _ => continue,
            };

            // A closed channel means the main loop is gone: stop, do not spin.
            if sender.send(app_event).is_err() {
                return;
            }
        }
    })
}

fn spawn_ticker(
    sender: Sender<AppEvent>,
    shutdown: Arc<Shutdown>,
    tick: Duration,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !shutdown.wait_timeout(tick) {
            if sender.send(AppEvent::Tick).is_err() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_arrive_without_any_input() {
        let events = EventSource::with_tick_interval(Duration::from_millis(10));
        assert!(matches!(events.next(), Ok(AppEvent::Tick)));
    }

    #[test]
    fn job_results_reach_the_loop() {
        let events = EventSource::with_tick_interval(Duration::from_secs(60));
        events
            .job_sender()
            .send(AppEvent::Job(JobResult {
                name: "example".to_string(),
            }))
            .expect("the receiver is alive");

        match events.next() {
            Ok(AppEvent::Job(result)) => assert_eq!(result.name, "example"),
            other => panic!("expected a job event, got {other:?}"),
        }
    }

    /// Dropping the source must not hang: both producers have to notice the stop
    /// request rather than block until the next keystroke or tick.
    #[test]
    fn dropping_the_source_joins_its_threads() {
        let events = EventSource::with_tick_interval(Duration::from_secs(60));
        drop(events);
    }
}
