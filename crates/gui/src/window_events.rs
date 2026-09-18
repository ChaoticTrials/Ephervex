//! Fanning the window's raw events out to more than one reader.
//!
//! The windowing backend keeps exactly one custom handler, and this build has
//! two things to read out of that layer which the toolkit does not model:
//! files dropped from the desktop, and which physical key was pressed. Neither
//! is a reason for the other not to exist, so both are registered here and
//! each sees every event.
//!
//! Readers run in the order they were added, and the first one to consume an
//! event ends it for the readers after it as well as for the toolkit. That is
//! the windowing layer's own rule for a consumed event, and it makes the order
//! meaningful: a reader that takes events away belongs last unless it only
//! takes the ones nothing else wants.

use slint::winit_030::{CustomApplicationHandler, EventResult, winit};
use winit::event_loop::ActiveEventLoop;

/// Every reader of the raw window events, in the order they were added.
#[derive(Default)]
pub struct WindowEventHandlers {
    readers: Vec<Box<dyn CustomApplicationHandler>>,
}

impl std::fmt::Debug for WindowEventHandlers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowEventHandlers")
            .field("readers", &self.readers.len())
            .finish()
    }
}

impl WindowEventHandlers {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a reader after the ones already registered.
    #[must_use]
    pub fn with(mut self, reader: impl CustomApplicationHandler + 'static) -> Self {
        self.readers.push(Box::new(reader));
        self
    }

    /// Offers one event to every reader until one consumes it.
    fn offer(
        &mut self,
        mut deliver: impl FnMut(&mut dyn CustomApplicationHandler) -> EventResult,
    ) -> EventResult {
        for reader in &mut self.readers {
            if matches!(deliver(reader.as_mut()), EventResult::PreventDefault) {
                return EventResult::PreventDefault;
            }
        }
        EventResult::Propagate
    }
}

impl CustomApplicationHandler for WindowEventHandlers {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        self.offer(|reader| reader.resumed(event_loop))
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        winit_window: Option<&winit::window::Window>,
        slint_window: Option<&slint::Window>,
        event: &winit::event::WindowEvent,
    ) -> EventResult {
        self.offer(|reader| {
            reader.window_event(event_loop, window_id, winit_window, slint_window, event)
        })
    }

    fn new_events(
        &mut self,
        event_loop: &ActiveEventLoop,
        cause: winit::event::StartCause,
    ) -> EventResult {
        self.offer(|reader| reader.new_events(event_loop, cause))
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) -> EventResult {
        self.offer(|reader| reader.device_event(event_loop, device_id, event.clone()))
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        self.offer(|reader| reader.about_to_wait(event_loop))
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        self.offer(|reader| reader.suspended(event_loop))
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        self.offer(|reader| reader.exiting(event_loop))
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        self.offer(|reader| reader.memory_warning(event_loop))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::cell::RefCell;

    use super::*;

    /// A reader that does nothing with the events it is offered.
    ///
    /// What each reader does with an event is its own file's concern; what is
    /// proved here is which readers are offered one at all, so the decisions
    /// are supplied by the test rather than taken from the readers.
    struct Silent;

    impl CustomApplicationHandler for Silent {}

    /// Offers one event, answering with `decisions` in order, and reports how
    /// many readers were reached.
    fn offer(readers: usize, decisions: &[EventResult]) -> (usize, EventResult) {
        let mut handlers = (0..readers).fold(WindowEventHandlers::new(), |all, _| all.with(Silent));
        let reached = RefCell::new(0_usize);
        let result = handlers.offer(|_| {
            let mut reached = reached.borrow_mut();
            let decision = decisions.get(*reached).map_or(EventResult::Propagate, |d| {
                if matches!(d, EventResult::PreventDefault) {
                    EventResult::PreventDefault
                } else {
                    EventResult::Propagate
                }
            });
            *reached += 1;
            decision
        });
        (reached.into_inner(), result)
    }

    #[test]
    fn every_registered_reader_sees_an_event_that_nobody_consumes() {
        let (reached, result) = offer(3, &[]);
        assert_eq!(reached, 3);
        assert!(matches!(result, EventResult::Propagate));
    }

    #[test]
    fn a_reader_that_consumes_an_event_ends_it_for_the_readers_after_it() {
        let (reached, result) = offer(3, &[EventResult::Propagate, EventResult::PreventDefault]);
        assert_eq!(reached, 2, "the third reader is never offered the event");
        assert!(
            matches!(result, EventResult::PreventDefault),
            "and the toolkit is told it was consumed"
        );
    }

    #[test]
    fn a_composer_with_no_readers_leaves_every_event_alone() {
        let (reached, result) = offer(0, &[EventResult::PreventDefault]);
        assert_eq!(reached, 0);
        assert!(matches!(result, EventResult::Propagate));
    }
}
