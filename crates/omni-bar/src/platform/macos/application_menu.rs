//! A dedicated process-menu target schedules app Quit; the current responder still handles editing actions.
//!
//! Menus retain no plugin/document identity; an explicit quit target avoids foreground resolution stacks during TerminateLater.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

define_class!(
    /// The process menu owns a payload-free target so the termination wait does not borrow foreground responders/controllers.
    // SAFETY: This NSObject subclass adds no ivars and preserves the single-argument action ABI and main-thread constraints.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct QuitMenuTarget;

    // SAFETY: Preserve initialized NSObject identity without overriding memory management or threading behavior.
    unsafe impl NSObjectProtocol for QuitMenuTarget {}

    impl QuitMenuTarget {
        #[unsafe(method(requestOmniTermination:))]
        /// Reserve quit once; enter the standard terminate:/delegate protocol only after the menu action returns.
        fn request_termination(&self, _sender: Option<&AnyObject>) {
            #[cfg(feature = "native-acceptance")]
            {
                // Actual menu-action evidence is independent of the later delegate's currentEvent query.
                super::acceptance_termination::record_menu_action();
            }
            if let Err(error) = super::termination::request_application_termination() {
                // Entry failure preserves the foreground without substituting direct terminate or forced exit.
                eprintln!("menu termination could not be scheduled: {error}");
            }
            #[cfg(feature = "native-acceptance")]
            {
                // The parent requires this scalar before delegate reply, exposing any wait reintroduced on the menu-action stack.
                super::acceptance_termination::record_menu_request_returned();
            }
        }
    }
);

impl QuitMenuTarget {
    /// Create a payload-free main-thread action target, owned by menu scope until the previous menu is restored.
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(());
        // SAFETY: Unit ivars are initialized; NSObject.init consumes the +1 allocation and returns the same main-thread type.
        unsafe { msg_send![super(allocated), init] }
    }
}

/// Restore the previous application menu if initialization fails or the run loop is explicitly stopped.
pub(super) struct ApplicationMenuScope<'a> {
    application: &'a NSApplication,
    previous: Option<Retained<NSMenu>>,
    _quit_target: Retained<QuitMenuTarget>,
}

impl<'a> ApplicationMenuScope<'a> {
    /// Install a minimal standard menu before the product run loop accepts keyboard events.
    pub(super) fn install(application: &'a NSApplication, mtm: MainThreadMarker) -> Self {
        // Keep the prior process menu restorable without retaining any foreground owner.
        let previous = application.mainMenu();
        // This target belongs to the process menu only and reuses no foreground/window delegate.
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
            "q",
            false,
            mtm,
        );
        // App-menu Quit uses a separate target; it is neither a system hotkey nor the plugin Back action.
        // An explicit target avoids nil-target resolution through the input responder chain and its temporary foreground claims.
        // SAFETY: Scope retains the main-thread target and releases it only after Drop restores the old menu.
        unsafe { quit.setTarget(Some(&*quit_target)) };

        // Localized menus retain standard selectors/keys; current native fields or document history handle editing.
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
            // A finite static menu registers ordinary responder actions, with no alternate editor state or cache.
            add_action(&edit, title, selector, key, shift, mtm);
        }
        // NSApplication retains the menu tree; local retained construction claims may end after publication.
        application.setMainMenu(Some(&main));
        Self {
            application,
            previous,
            _quit_target: quit_target,
        }
    }
}

/// Build responder-chain menu items by default; callers may install explicit targets for app-wide actions.
fn add_action(
    menu: &NSMenu,
    title: &str,
    selector: Sel,
    key: &str,
    shift: bool,
    mtm: MainThreadMarker,
) -> Retained<NSMenuItem> {
    // SAFETY: Callers supply declared project/AppKit single-object action selectors matching NSMenuItem's ABI.
    // Initial nil targets use the responder chain; app Quit then installs its scope-owned explicit target.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(selector),
            &NSString::from_str(key),
        )
    };
    let modifiers = if shift {
        // Standard Redo is Command-Shift-Z; Shift remains explicit rather than relying on an uppercase key alias.
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift
    } else {
        // All other declared actions use their standard exact Command-letter equivalent.
        NSEventModifierFlags::Command
    };
    item.setKeyEquivalentModifierMask(modifiers);
    menu.addItem(&item);
    // The menu owns the item already; the returned local claim permits target configuration without retaining a foreground graph.
    item
}

impl Drop for ApplicationMenuScope<'_> {
    fn drop(&mut self) {
        // A stopped/failed initialization restores native process state before this scope releases its old menu.
        self.application.setMainMenu(self.previous.as_deref());
    }
}
