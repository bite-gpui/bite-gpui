use crate::{PlatformDispatcher, Priority, RunnableVariant};
use gpui_platform::TestDispatcher;
use scheduler::{Clock, Instant, Scheduler};
use std::time::Duration;

// `TestDispatcher` itself moved down into `gpui_platform`; its dispatch behaviour is
// still defined by `PlatformDispatcher`, which lives here, so the impl stays behind and
// reaches the scheduler through the type's public accessors.
impl PlatformDispatcher for TestDispatcher {
    fn is_main_thread(&self) -> bool {
        self.scheduler().is_main_thread()
    }

    fn now(&self) -> Instant {
        self.scheduler().clock().now()
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        self.scheduler()
            .schedule_background_with_priority(runnable, priority);
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
        self.scheduler().schedule_local(self.session_id(), runnable);
    }

    fn dispatch_after(&self, _duration: Duration, _runnable: RunnableVariant) {
        panic!(
            "dispatch_after should not be called in tests. \
            Use BackgroundExecutor::timer() which uses the scheduler's native timer."
        );
    }

    fn as_test(&self) -> Option<&TestDispatcher> {
        Some(self)
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(move || {
            f();
        });
    }
}
