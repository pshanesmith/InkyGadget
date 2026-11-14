//! The way button events cross from the button thread to the Matter node's async tasks.
//!
//! One small queue per button: the button thread pushes (never blocks), and the Switch handler of that button awaits the
//! next event. Only std types are used, so the waker of whatever executor runs the Matter node is woken directly.

use crate::buttons::{Button, SwitchSink};
use crate::switchlogic::SwitchEvent;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::task::{Poll, Waker};

/// Events kept per button when nobody is listening yet (the node still starting): older ones are dropped.
const QUEUE: usize = 32;

#[derive(Default)]
struct Queue {
    events: VecDeque<SwitchEvent>,
    waker: Option<Waker>,
}

#[derive(Default)]
pub struct ButtonBus {
    queues: [Mutex<Queue>; 4],
    down: [AtomicBool; 4],
}

impl ButtonBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is the button held (the Switch cluster's `CurrentPosition` is 1)?
    pub fn is_down(&self, button: usize) -> bool {
        self.down[button].load(Ordering::Relaxed)
    }

    /// The next event of this button, waiting for it if there is none yet.
    pub async fn recv(&self, button: usize) -> SwitchEvent {
        poll_fn(|cx| {
            let mut q = self.queues[button].lock().unwrap_or_else(PoisonError::into_inner);
            match q.events.pop_front() {
                Some(e) => Poll::Ready(e),
                None => {
                    q.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await
    }
}

impl SwitchSink for ButtonBus {
    fn send(&self, button: Button, event: SwitchEvent, down: bool) {
        let i = button.index();
        self.down[i].store(down, Ordering::Relaxed);
        let mut q = self.queues[i].lock().unwrap_or_else(PoisonError::into_inner);
        if q.events.len() >= QUEUE {
            q.events.pop_front();
        }
        q.events.push_back(event);
        if let Some(w) = q.waker.take() {
            w.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn events_arrive_in_order_and_position_follows() {
        let bus = ButtonBus::new();
        bus.send(Button::B, SwitchEvent::InitialPress, true);
        assert!(bus.is_down(1) && !bus.is_down(0));
        bus.send(Button::B, SwitchEvent::ShortRelease, false);
        assert!(!bus.is_down(1));
        let got = (futures_lite::future::block_on(bus.recv(1)), futures_lite::future::block_on(bus.recv(1)));
        assert_eq!(got, (SwitchEvent::InitialPress, SwitchEvent::ShortRelease));
    }

    #[test]
    fn a_waiting_receiver_is_woken_from_another_thread() {
        let bus = Arc::new(ButtonBus::new());
        let b = bus.clone();
        let t = std::thread::spawn(move || futures_lite::future::block_on(b.recv(3)));
        std::thread::sleep(std::time::Duration::from_millis(100));
        bus.send(Button::D, SwitchEvent::LongPress, true);
        assert_eq!(t.join().unwrap(), SwitchEvent::LongPress);
    }

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let bus = ButtonBus::new();
        for _ in 0..40 {
            bus.send(Button::A, SwitchEvent::InitialPress, true);
        }
        bus.send(Button::A, SwitchEvent::LongPress, true);
        assert_eq!(bus.queues[0].lock().unwrap().events.len(), QUEUE);
        assert_eq!(*bus.queues[0].lock().unwrap().events.back().unwrap(), SwitchEvent::LongPress);
    }
}
