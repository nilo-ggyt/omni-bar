//! Schedule explicit process termination while editing stays with the current native responder.
//!
//! A dedicated quit target avoids retaining plugin/document responders during asynchronous termination.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

define_class!(
    /// A menu-owned target with no business payload or borrowed foreground controller.
    // SAFETY: the subclass adds no ivars and preserves the single-object action ABI on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct QuitMenuTarget;

    // SAFETY: standard NSObject identity, memory management and main-thread behavior are inherited.
    unsafe impl NSObjectProtocol for QuitMenuTarget {}

    impl QuitMenuTarget {
        #[unsafe(method(requestOmniTermination:))]
        /// Schedule termination after this action returns, preserving the native delegate transaction.
        fn request_termination(&self, sender: Option<&AnyObject>) {
            #[cfg(feature = "native-acceptance")]
            {
                // Observe the real menu action only; verification must not synthesize an unwanted quit shortcut.
                super::acceptance_termination::record_menu_action(sender);
            }
            #[cfg(not(feature = "native-acceptance"))]
            let _ = sender;
            if let Err(error) = super::termination::request_application_termination() {
                // Scheduling failure preserves the current foreground instead of bypassing its save transaction.
                eprintln!("menu termination could not be scheduled: {error}");
            }
            #[cfg(feature = "native-acceptance")]
            {
                // The parent observes this return before the delegate reply to detect waiting on the action stack.
                super::acceptance_termination::record_menu_request_returned();
            }
        }
    }
);

impl QuitMenuTarget {
    /// Create the main-thread action target owned until this scope restores the previous menu.
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(());
        // SAFETY: initialized unit ivars precede NSObject.init, which consumes the +1 subclass allocation.
        unsafe { msg_send![super(allocated), init] }
    }
}

/// Restore the previous application menu on initialization failure or an explicit event-loop stop.
pub(super) struct ApplicationMenuScope<'a> {
    application: &'a NSApplication,
    previous: Option<Retained<NSMenu>>,
    _quit_target: Retained<QuitMenuTarget>,
}

impl<'a> ApplicationMenuScope<'a> {
    /// Install the required menu before the product event loop begins receiving input.
    pub(super) fn install(application: &'a NSApplication, mtm: MainThreadMarker) -> Self {
        // Retain the original process menu without acquiring any foreground business object.
        let previous = application.mainMenu();
        // The process menu owns its quit target independently from every window delegate.
        let quit_target = QuitMenuTarget::new(mtm);
        let main = NSMenu::new(mtm);
        let application_menu =
            NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Omni Bar"));
        let application_item = NSMenuItem::new(mtm);
        application_item.setTitle(&NSString::from_str("Omni Bar"));
        application_item.setSubmenu(Some(&application_menu));
        main.addItem(&application_item);
        let quit = add_action(
            &application_menu,
            "退出 Omni Bar",
            sel!(requestOmniTermination:),
            // Explicit menu selection enters the existing transaction without registering Cmd+Q or another key.
            "",
            false,
            mtm,
        );
        // Process quit and plugin return keep their distinct intents and the existing save/release ordering.
        // A concrete target avoids retaining the plugin window through responder-chain target resolution.
        // SAFETY: this scope retains the main-thread target until Drop restores the original menu.
        unsafe { quit.setTarget(Some(&*quit_target)) };

        // Chinese menu labels use existing selectors and editing keys, handled by the actual focused editor.
        let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("编辑"));
        let edit_item = NSMenuItem::new(mtm);
        edit_item.setTitle(&NSString::from_str("编辑"));
        edit_item.setSubmenu(Some(&edit));
        main.addItem(&edit_item);
        for (title, selector, key, shift) in [
            ("撤销", sel!(undo:), "z", false),
            ("重做", sel!(redo:), "z", true),
            ("剪切", sel!(cut:), "x", false),
            ("复制", sel!(copy:), "c", false),
            ("粘贴", sel!(paste:), "v", false),
            ("全选", sel!(selectAll:), "a", false),
        ] {
            // Register responder actions only; document state and undo history stay with their real editor.
            add_action(&edit, title, selector, key, shift, mtm);
        }
        // NSApplication retains the published menu tree, so local construction references can now end.
        application.setMainMenu(Some(&main));
        Self {
            application,
            previous,
            _quit_target: quit_target,
        }
    }
}

/// Create a responder-chain item; the caller binds process-level actions to their concrete target.
fn add_action(
    menu: &NSMenu,
    title: &str,
    selector: Sel,
    key: &str,
    shift: bool,
    mtm: MainThreadMarker,
) -> Retained<NSMenuItem> {
    // SAFETY: callers supply declared selectors with the single-object NSMenuItem action ABI.
    // Nil initially uses the responder chain; the quit item is subsequently bound to the scope-owned target.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(selector),
            &NSString::from_str(key),
        )
    };
    let modifiers = if shift {
        // Redo explicitly preserves Command-Shift-Z without relying on an uppercase-character alias.
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift
    } else {
        // Other editing actions keep their Command chords; an empty quit key registers no shortcut.
        NSEventModifierFlags::Command
    };
    item.setKeyEquivalentModifierMask(modifiers);
    menu.addItem(&item);
    // The menu already owns this item; the returned handle is used only to bind its action target.
    item
}

impl Drop for ApplicationMenuScope<'_> {
    fn drop(&mut self) {
        // Restore native menu state before releasing the previous-menu claim on every stopped or failed exit.
        self.application.setMainMenu(self.previous.as_deref());
    }
}
