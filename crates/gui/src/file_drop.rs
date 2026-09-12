//! Accepting files dropped onto the window from the desktop.
//!
//! # Why this is not Slint's own drag and drop
//!
//! Slint has `DragArea` and `DropArea`, and they carry a `data-transfer` value
//! that holds plain text or an image. Neither is a file, and the desktop
//! backend never turns an external drag into one, so a `DropArea` only ever
//! sees drags that started inside this application. What a user drops on the
//! window is a path, and the only place a path exists is the windowing event
//! Slint does not model.
//!
//! So the events are read one layer down, through the winit backend's own
//! handler hook. That hook is behind a Slint feature the project documents as
//! unstable across minor releases, which is why everything built on it is here:
//! one file, a handful of event kinds, and no launcher logic.
//!
//! # Why a drop is collected rather than delivered as it arrives
//!
//! Dropping six jars produces six `DroppedFile` events and no event saying the
//! drop is over. Delivered one at a time they would read as six drops, and six
//! separate answers to "this name is taken" is not a question anybody wants
//! asked. `about_to_wait` runs once after the windowing system's events are
//! drained, so it is where one gesture has finished arriving -- no timer, and
//! nothing guessing at how long a drop takes.
//!
//! # What this does not do
//!
//! Nothing about the drop is trusted beyond "the user named these paths". A
//! pack archive goes through the same analysis a file chosen from the dialog
//! does, which detects the format from the archive's contents rather than from
//! its name, and a jar goes through the same validation either way.
//!
//! Winit implements file drops on X11 and not on Wayland, so this works under
//! X11 and XWayland and is inert on a native Wayland session, where the dialog
//! remains the way in. The same is true of this crate's window-aspect hook.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::winit_030::{CustomApplicationHandler, EventResult, winit};

/// What the desktop is doing with a file over the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDrop {
    /// A file is being held over the window.
    Hovering,
    /// It was taken away again, or the drag was canceled.
    Left,
    /// It was let go. Every path of one gesture, in the order winit reported
    /// them, and never empty.
    Dropped(Vec<PathBuf>),
}

/// The seam between the windowing handler and the frontend.
///
/// Two-part because the handler has to exist before the window does: the
/// backend is chosen first, and only then is there a window to tell about a
/// drop. Until something is listening, drops are discarded rather than queued;
/// a file dropped on a window that is still starting has no attempt to belong
/// to.
/// Whatever the frontend wants told about a drop.
type Listener = Rc<dyn Fn(FileDrop)>;

#[derive(Clone, Default)]
pub struct FileDrops {
    listener: Rc<RefCell<Option<Listener>>>,
}

impl std::fmt::Debug for FileDrops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDrops")
            .field("listening", &self.listener.borrow().is_some())
            .finish()
    }
}

impl FileDrops {
    /// Sends every drop to `listener`, replacing any previous one.
    pub fn listen(&self, listener: impl Fn(FileDrop) + 'static) {
        *self.listener.borrow_mut() = Some(Rc::new(listener));
    }

    /// Delivers one event, if anything is listening.
    ///
    /// The listener is taken out of the cell before it runs, so that a listener
    /// which ends up back here cannot panic on the borrow.
    fn deliver(&self, drop: FileDrop) {
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener(drop);
        }
    }
}

/// Reads file drops out of the windowing events and hands them on.
#[derive(Debug)]
pub struct FileDropHandler {
    drops: FileDrops,
    /// The paths of the drop currently arriving, delivered together once the
    /// windowing system has nothing left to say.
    collecting: Vec<PathBuf>,
}

impl FileDropHandler {
    #[must_use]
    pub const fn new(drops: FileDrops) -> Self {
        Self {
            drops,
            collecting: Vec::new(),
        }
    }
}

impl FileDropHandler {
    /// Hands on the paths of the drop that has finished arriving, if one has.
    ///
    /// Nothing is delivered for an empty collection: this runs every time the
    /// window settles, and a listener told about a drop nobody made would act
    /// on a gesture that did not happen.
    fn flush(&mut self) {
        if !self.collecting.is_empty() {
            self.drops
                .deliver(FileDrop::Dropped(std::mem::take(&mut self.collecting)));
        }
    }

    /// Abandons a drag that was taken away again.
    fn cancel(&mut self) {
        self.collecting.clear();
        self.drops.deliver(FileDrop::Left);
    }
}

impl CustomApplicationHandler for FileDropHandler {
    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _winit_window: Option<&winit::window::Window>,
        _slint_window: Option<&slint::Window>,
        event: &winit::event::WindowEvent,
    ) -> EventResult {
        match event {
            winit::event::WindowEvent::HoveredFile(_) => self.drops.deliver(FileDrop::Hovering),
            winit::event::WindowEvent::HoveredFileCancelled => self.cancel(),
            winit::event::WindowEvent::DroppedFile(path) => self.collecting.push(path.clone()),
            _ => {}
        }
        // Never consumed. These events are Slint's to see as well, and reading
        // one must not change how the rest of the window behaves.
        EventResult::Propagate
    }

    /// Delivers whatever the drop that just finished was carrying.
    fn about_to_wait(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) -> EventResult {
        self.flush();
        EventResult::Propagate
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::cell::RefCell;

    use super::*;

    /// Everything a listener was told, in order.
    fn recording() -> (FileDrops, Rc<RefCell<Vec<FileDrop>>>) {
        let drops = FileDrops::default();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let record = Rc::clone(&seen);
        drops.listen(move |drop| record.borrow_mut().push(drop));
        (drops, seen)
    }

    #[test]
    fn every_file_of_one_gesture_arrives_as_a_single_drop() {
        // Winit reports one event per file and never says the drop is over.
        // Delivered one at a time these would read as three separate drops, and
        // three separate answers to one question is not a dialog worth having.
        let (drops, seen) = recording();
        let mut handler = FileDropHandler::new(drops);
        handler.collecting.push(PathBuf::from("/tmp/one.jar"));
        handler.collecting.push(PathBuf::from("/tmp/two.jar"));
        handler.collecting.push(PathBuf::from("/tmp/three.jar"));

        handler.flush();

        assert_eq!(
            seen.borrow().as_slice(),
            [FileDrop::Dropped(vec![
                PathBuf::from("/tmp/one.jar"),
                PathBuf::from("/tmp/two.jar"),
                PathBuf::from("/tmp/three.jar"),
            ])]
        );
    }

    #[test]
    fn a_settled_window_with_nothing_dropped_says_nothing() {
        // `about_to_wait` runs constantly. A listener told about an empty drop
        // every time the window goes idle would act on gestures nobody made.
        let (drops, seen) = recording();
        let mut handler = FileDropHandler::new(drops);

        handler.flush();
        handler.flush();

        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_drag_taken_away_again_leaves_nothing_to_deliver() {
        let (drops, seen) = recording();
        let mut handler = FileDropHandler::new(drops);
        handler.collecting.push(PathBuf::from("/tmp/one.jar"));

        handler.cancel();
        handler.flush();

        assert_eq!(
            seen.borrow().as_slice(),
            [FileDrop::Left],
            "a gesture that did not finish must not arrive as one that did"
        );
    }

    #[test]
    fn one_drop_does_not_leave_its_paths_behind_for_the_next() {
        let (drops, seen) = recording();
        let mut handler = FileDropHandler::new(drops);
        handler.collecting.push(PathBuf::from("/tmp/one.jar"));
        handler.flush();

        handler.collecting.push(PathBuf::from("/tmp/two.jar"));
        handler.flush();

        assert_eq!(
            seen.borrow().as_slice(),
            [
                FileDrop::Dropped(vec![PathBuf::from("/tmp/one.jar")]),
                FileDrop::Dropped(vec![PathBuf::from("/tmp/two.jar")]),
            ]
        );
    }
}
