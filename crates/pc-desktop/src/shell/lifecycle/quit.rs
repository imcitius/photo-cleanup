//! A single quit attempt can be promoted by a terminal signal while a
//! confirmation is pending. Cancellation and promotion share one lock so
//! a late dialog answer cannot discard an already received signal.
use super::lock;
use std::future::Future;
use std::sync::Mutex;
use tokio::sync::watch;

#[derive(Default)]
pub(in crate::shell) struct Requests(Mutex<Option<watch::Sender<bool>>>);

impl Requests {
    pub(in crate::shell) fn pending(&self) -> bool {
        lock(&self.0).is_some()
    }

    pub(super) fn request(&self, confirmed: bool) -> Option<watch::Receiver<bool>> {
        let mut current = lock(&self.0);
        if let Some(sender) = current.as_ref() {
            if confirmed {
                sender.send_replace(true);
            }
            return None;
        }
        let (sender, receiver) = watch::channel(confirmed);
        *current = Some(sender);
        Some(receiver)
    }

    /// False means that a signal won the race with the dialog's Cancel.
    pub(super) fn cancel(&self) -> bool {
        let mut current = lock(&self.0);
        if current.as_ref().is_some_and(|sender| *sender.borrow()) {
            return false;
        }
        *current = None;
        true
    }
}

pub(super) async fn decide(
    receiver: &mut watch::Receiver<bool>,
    dialog: impl Future<Output = bool>,
) -> bool {
    tokio::select! {
        biased;
        result = receiver.wait_for(|confirmed| *confirmed) => result.is_ok(),
        result = dialog => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_signal_promotes_an_open_dialog_without_a_second_quit_task() {
        let requests = Requests::default();
        let mut receiver = requests.request(false).unwrap();
        let decision = decide(&mut receiver, std::future::pending());
        tokio::pin!(decision);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut decision)
                .await
                .is_err()
        );
        assert!(requests.request(true).is_none());
        assert!(requests.request(true).is_none());
        assert!(decision.await);
        assert!(!requests.cancel());
        assert!(requests.pending());
    }

    #[tokio::test]
    async fn cancel_allows_a_later_signal_to_start_a_new_quit_attempt() {
        let requests = Requests::default();
        let mut receiver = requests.request(false).unwrap();
        assert!(!decide(&mut receiver, async { false }).await);
        assert!(requests.cancel());
        assert!(!requests.pending());
        let mut signal = requests.request(true).unwrap();
        assert!(decide(&mut signal, std::future::pending()).await);
    }

    #[tokio::test]
    async fn a_signal_between_dialog_answer_and_cancel_is_not_lost() {
        let requests = Requests::default();
        let mut receiver = requests.request(false).unwrap();
        assert!(!decide(&mut receiver, async { false }).await);
        assert!(requests.request(true).is_none());
        assert!(!requests.cancel());
    }
}
