//! Revocable access for work that outlives an HTTP request or encryption lock.
use std::sync::OnceLock;
use tokio::sync::watch;

#[derive(Clone, Copy, Default)]
struct State {
    generation: u64,
    locked: bool,
}

static ACCESS: OnceLock<watch::Sender<State>> = OnceLock::new();

fn access() -> &'static watch::Sender<State> {
    ACCESS.get_or_init(|| watch::channel(State::default()).0)
}

pub(crate) fn set_locked(locked: bool) {
    access().send_modify(|state| {
        state.generation = state.generation.wrapping_add(1);
        state.locked = locked;
    });
}

#[derive(Clone)]
pub(crate) struct Lease {
    receiver: watch::Receiver<State>,
    generation: u64,
}

pub(crate) fn lease() -> Result<Lease, String> {
    let receiver = access().subscribe();
    let state = *receiver.borrow();
    if state.locked {
        return Err("Encrypted local data is locked.".into());
    }
    Ok(Lease {
        receiver,
        generation: state.generation,
    })
}

impl Lease {
    pub(crate) fn valid(&self) -> bool {
        let state = *self.receiver.borrow();
        !state.locked && state.generation == self.generation
    }

    pub(crate) async fn revoked(&mut self) {
        while self.valid() {
            if self.receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlock_never_revives_an_old_lease() {
        let (sender, receiver) = watch::channel(State::default());
        let mut lease = Lease {
            receiver,
            generation: 0,
        };
        assert!(lease.valid());
        sender.send_replace(State {
            generation: 1,
            locked: true,
        });
        sender.send_replace(State {
            generation: 2,
            locked: false,
        });
        assert!(!lease.valid());
        tokio::time::timeout(std::time::Duration::from_millis(50), lease.revoked())
            .await
            .unwrap();
    }
}
