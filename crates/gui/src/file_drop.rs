//! Accepting a modpack archive dropped onto the window from the desktop.
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
//! one file, three event kinds, and no launcher logic.
//!
//! # What this does not do
//!
//! Nothing about the drop is trusted beyond "the user named this path". The
//! archive goes through the same analysis a file chosen from the dialog does,
//! which detects the pack format from the archive's contents rather than from
//! its name.
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
    /// It was let go, at this path.
    Dropped(PathBuf),
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
}

impl FileDropHandler {
    #[must_use]
    pub const fn new(drops: FileDrops) -> Self {
        Self { drops }
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
            winit::event::WindowEvent::HoveredFileCancelled => self.drops.deliver(FileDrop::Left),
            winit::event::WindowEvent::DroppedFile(path) => {
                self.drops.deliver(FileDrop::Dropped(path.clone()));
            }
            _ => {}
        }
        // Never consumed. These events are Slint's to see as well, and reading
        // one must not change how the rest of the window behaves.
        EventResult::Propagate
    }
}
