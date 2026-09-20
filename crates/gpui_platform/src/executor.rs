use async_task::Runnable;
use scheduler::RunnableMeta;

/// Restores the platform's timer resolution when dropped.
#[doc(hidden)]
pub type TimerResolutionGuard = gpui_util::Deferred<Box<dyn FnOnce() + Send>>;

/// Type alias for runnables with metadata.
/// Previously an enum with a single variant, now simplified to a direct type alias.
#[doc(hidden)]
pub type RunnableVariant = Runnable<RunnableMeta>;
