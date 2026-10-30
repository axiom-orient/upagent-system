use tokio_util::sync::CancellationToken;

/// Cooperative cancellation for one task or one explicitly grouped batch.
///
/// Provider and extension futures observe the Tokio token directly. The effect
/// adapter creates a private RAX cancellation token per operation and bridges
/// this signal into it, so a deadline or cancellation cannot leak across
/// concurrently running tasks.
#[derive(Debug, Clone)]
pub struct TaskCancellation {
    token: CancellationToken,
}

impl Default for TaskCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cloned_handles_observe_the_same_cancellation() {
        let cancellation = TaskCancellation::new();
        let observer = cancellation.clone();

        cancellation.cancel();
        observer.cancelled().await;

        assert!(observer.is_cancelled());
    }
}
