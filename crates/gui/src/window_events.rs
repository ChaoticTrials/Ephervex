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
//!
//! The application has more than one window, and the readers act on the main
//! one: a drop is routed by the main window's page and a key capture is armed
//! from its editors. Window events are therefore offered only when they come
//! from the main window.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::winit_030::{CustomApplicationHandler, EventResult, winit};
use winit::event_loop::ActiveEventLoop;

/// Whether a window is the one the readers act for.
type Belongs = Box<dyn Fn(&slint::Window) -> bool>;

/// Names the main window to the readers once it exists.
///
/// The handler has to be registered before any window is created, so it is
/// handed this and the window is attached afterwards. Until then no window
/// counts as the main one.
#[derive(Clone, Default)]
pub struct MainWindow {
    belongs: Rc<RefCell<Option<Belongs>>>,
}

impl std::fmt::Debug for MainWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainWindow")
            .field("attached", &self.belongs.borrow().is_some())
            .finish()
    }
}

impl MainWindow {
    /// Makes `component`'s window the main one, replacing any previous choice.
    ///
    /// The component is held weakly, so naming it does not keep it alive.
    pub fn attach<C: ComponentHandle + 'static>(&self, component: &C) {
        let weak = component.as_weak();
        *self.belongs.borrow_mut() = Some(Box::new(move |window| {
            weak.upgrade()
                .is_some_and(|component| std::ptr::eq(component.window(), window))
        }));
    }

    /// Whether `window`, the one an event came from, is the main window.
    ///
    /// An event from a window Slint does not know has no window to compare, so
    /// it is not the main window's.
    fn is_main(&self, window: Option<&slint::Window>) -> bool {
        let belongs = self.belongs.borrow();
        matches!((belongs.as_ref(), window), (Some(belongs), Some(window)) if belongs(window))
    }
}

/// Every reader of the raw window events, in the order they were added.
#[derive(Default)]
pub struct WindowEventHandlers {
    main: MainWindow,
    readers: Vec<Box<dyn CustomApplicationHandler>>,
}

impl std::fmt::Debug for WindowEventHandlers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowEventHandlers")
            .field("main", &self.main)
            .field("readers", &self.readers.len())
            .finish()
    }
}

impl WindowEventHandlers {
    /// Readers that see the events of `main` and no other window.
    #[must_use]
    pub fn new(main: MainWindow) -> Self {
        Self {
            main,
            readers: Vec::new(),
        }
    }

    /// Adds a reader after the ones already registered.
    #[must_use]
    pub fn with(mut self, reader: impl CustomApplicationHandler + 'static) -> Self {
        self.readers.push(Box::new(reader));
        self
    }

    /// Offers an event of `window` to the readers, unless it is not the main
    /// window's.
    fn offer_from(
        &mut self,
        window: Option<&slint::Window>,
        deliver: impl FnMut(&mut dyn CustomApplicationHandler) -> EventResult,
    ) -> EventResult {
        if self.main.is_main(window) {
            self.offer(deliver)
        } else {
            EventResult::Propagate
        }
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
        self.offer_from(slint_window, |reader| {
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
        let mut handlers = (0..readers)
            .fold(WindowEventHandlers::new(MainWindow::default()), |all, _| {
                all.with(Silent)
            });
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

    #[test]
    fn no_window_is_the_main_one_before_the_main_window_exists() {
        i_slint_backend_testing::init_no_event_loop();
        let window = crate::ui::AppWindow::new().expect("window");

        assert!(!MainWindow::default().is_main(Some(window.window())));
    }

    #[test]
    fn only_the_attached_window_is_the_main_one() {
        i_slint_backend_testing::init_no_event_loop();
        let main_window = crate::ui::AppWindow::new().expect("window");
        let preview = crate::ui::WindowSizePreview::new().expect("window");
        let other_main = crate::ui::AppWindow::new().expect("window");
        let main = MainWindow::default();
        main.attach(&main_window);

        assert!(main.is_main(Some(main_window.window())));
        assert!(
            !main.is_main(Some(preview.window())),
            "the sizing window's events are not the main window's"
        );
        assert!(!main.is_main(Some(other_main.window())));
    }

    #[test]
    fn an_event_from_a_window_slint_does_not_know_is_not_the_main_windows() {
        i_slint_backend_testing::init_no_event_loop();
        let window = crate::ui::AppWindow::new().expect("window");
        let main = MainWindow::default();
        main.attach(&window);

        assert!(!main.is_main(None));
    }

    #[test]
    fn a_closed_main_window_leaves_no_window_as_the_main_one() {
        i_slint_backend_testing::init_no_event_loop();
        let main = MainWindow::default();
        let survivor = crate::ui::AppWindow::new().expect("window");
        {
            let gone = crate::ui::AppWindow::new().expect("window");
            main.attach(&gone);
        }

        assert!(!main.is_main(Some(survivor.window())));
    }

    #[test]
    fn a_window_that_is_not_the_main_one_reaches_no_reader() {
        i_slint_backend_testing::init_no_event_loop();
        let main_window = crate::ui::AppWindow::new().expect("window");
        let preview = crate::ui::WindowSizePreview::new().expect("window");
        let main = MainWindow::default();
        main.attach(&main_window);
        let mut handlers = WindowEventHandlers::new(main).with(Silent);
        let mut reached = 0_usize;

        // The deliverer consumes every event it is offered, so the answer
        // also shows whether the toolkit would be told the event was taken.
        let from_preview = handlers.offer_from(Some(preview.window()), |_| {
            reached += 1;
            EventResult::PreventDefault
        });
        assert_eq!(reached, 0);
        assert!(matches!(from_preview, EventResult::Propagate));

        let from_main = handlers.offer_from(Some(main_window.window()), |_| {
            reached += 1;
            EventResult::PreventDefault
        });
        assert_eq!(reached, 1);
        assert!(matches!(from_main, EventResult::PreventDefault));
    }
}
