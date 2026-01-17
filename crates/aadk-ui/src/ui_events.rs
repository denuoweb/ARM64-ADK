use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::commands::AppEvent;

pub(crate) const DEFAULT_EVENT_QUEUE_SIZE: usize = 512;

#[derive(Clone)]
pub(crate) struct UiEventSender {
    inner: Arc<UiEventQueue>,
}

pub(crate) struct UiEventQueue {
    queue: Mutex<VecDeque<AppEvent>>,
    notify: mpsc::Sender<()>,
    max_len: usize,
}

impl UiEventQueue {
    pub(crate) fn new(max_len: usize) -> (Arc<Self>, mpsc::Receiver<()>) {
        let (notify, notify_rx) = mpsc::channel(1);
        (
            Arc::new(Self {
                queue: Mutex::new(VecDeque::new()),
                notify,
                max_len,
            }),
            notify_rx,
        )
    }

    pub(crate) fn sender(self: &Arc<Self>) -> UiEventSender {
        UiEventSender {
            inner: Arc::clone(self),
        }
    }

    pub(crate) fn drain(&self) -> Vec<AppEvent> {
        let mut queue = self.queue.lock().unwrap();
        queue.drain(..).collect()
    }

    fn push(&self, event: AppEvent) -> bool {
        let mut queue = self.queue.lock().unwrap();
        let was_empty = queue.is_empty();

        if matches!(event, AppEvent::HomeProgress { .. }) {
            if let Some(existing) = queue
                .iter_mut()
                .find(|ev| matches!(ev, AppEvent::HomeProgress { .. }))
            {
                *existing = event;
                return false;
            }
        }

        if queue.len() >= self.max_len {
            if let Some(pos) = queue
                .iter()
                .position(|ev| matches!(ev, AppEvent::Log { .. }))
            {
                queue.remove(pos);
            } else if matches!(event, AppEvent::Log { .. }) {
                return false;
            } else {
                queue.pop_front();
            }
        }

        queue.push_back(event);
        if was_empty {
            let _ = self.notify.try_send(());
        }
        true
    }
}

impl UiEventSender {
    pub(crate) fn send(&self, event: AppEvent) -> Result<(), ()> {
        self.inner.push(event);
        Ok(())
    }
}
