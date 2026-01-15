use tokio::sync::watch;

pub(crate) fn cancel_requested(cancel_rx: Option<&watch::Receiver<bool>>) -> bool {
    cancel_rx.map(|rx| *rx.borrow()).unwrap_or(false)
}
