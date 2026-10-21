//! Generic SDK activation, borrowed-view mounting, and reentrancy-safe session ownership.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use objc2::{MainThreadMarker, rc::autoreleasepool};
use objc2_app_kit::NSPasteboardTypeString;
use objc2_foundation::NSString;
use omni_plugin_sdk::{
    ModifierFlags, OmniBarHostContext, PluginActivation, PluginCloseIntent, PluginCloseRequest,
    PluginCloseScope, PluginCommandTarget, PluginError, PluginHostLifecycle, PluginInvocation,
    PluginNativeView, PluginRegistration, PluginSession,
};

use crate::application::termination::TerminationTicket;
use crate::{ForegroundSurface, ResumeTarget, SurfaceCloseIntent};

thread_local! {
    /// The host owns one session only after successful synchronous activation.
    static SESSION: RefCell<Option<Box<dyn PluginSession>>> = const { RefCell::new(None) };
    /// This copied handle is a borrow; it never retains the plugin's NSView.
    static MOUNTED_VIEW: Cell<Option<PluginNativeView>> = const { Cell::new(None) };
    /// A monotonic host-only epoch rejects callbacks from an earlier instance of the same plugin.
    static LAST_GENERATION: Cell<u64> = const { Cell::new(0) };
    /// Static command identity alone cannot distinguish consecutive sessions of the same plugin.
    static OWNER_GENERATION: Cell<Option<u64>> = const { Cell::new(None) };
    /// Completion may reenter while an activation or close handler is still on the stack.
    static CALL_IN_FLIGHT: Cell<bool> = const { Cell::new(false) };
    /// Session Drop may run arbitrary plugin code before the foreground barrier is consumed.
    static FINALIZATION_IN_FLIGHT: Cell<bool> = const { Cell::new(false) };
    /// Recursive close coalesces only the same intent; activation has no accepted close direction.
    static CLOSE_IN_FLIGHT: Cell<Option<PluginCloseRequest>> = const { Cell::new(None) };
    /// Exact accepted intent/scope is independent of the Box's temporary local method-call ownership.
    static ACCEPTED_CLOSE: Cell<Option<PluginCloseRequest>> = const { Cell::new(None) };
    /// A terminal receipt is valid only in the exact accepted delegate transaction, never a later Quit retry.
    static ACCEPTED_TERMINATION: Cell<Option<TerminationTicket>> = const { Cell::new(None) };
    /// Delay final ownership release until that borrowed method invocation has returned.
    static DEFERRED_FINISH: Cell<Option<(u64, PluginCommandTarget, PluginCloseRequest)>> = const { Cell::new(None) };
}

/// Per-activation capability context; paths and lifecycle resources end with activation.
struct HostContext {
    generation: u64,
    storage: super::NativePluginStorageLocations,
    lifecycle: Rc<dyn PluginHostLifecycle>,
}

impl OmniBarHostContext for HostContext {
    /// Borrow the activation-selected vault identity without opening or enumerating it.
    fn workspace_root(&self) -> &Path {
        // Borrow the authority path without creating or enumerating the vault.
        &self.storage.workspace_root
    }

    /// Borrow the app-private root under which this activation may acquire foreground state.
    fn application_data_root(&self) -> &Path {
        // The plugin creates only explicitly activated resources under this authority.
        &self.storage.application_support
    }

    /// Accept one main-thread borrowed view only for this current activation epoch.
    fn mount_native_view(&mut self, view: PluginNativeView) -> Result<(), PluginError> {
        // Native pointer authority requires both this activation epoch and the actual AppKit thread.
        let _mtm = main_thread()?;
        require_generation(self.generation)?;
        if MOUNTED_VIEW.get().is_some() {
            // Preserve the existing borrow instead of replacing an already-mounted graph.
            return Err(PluginError::new(
                "surface_already_mounted",
                "宿主前台容器已经挂载原生 view",
            ));
        }
        // Store only the borrow after the plugin has proved its lifetime/main-thread contract.
        MOUNTED_VIEW.set(Some(view));
        Ok(())
    }

    /// Return explicit transient system text without retaining a host clipboard cache.
    fn read_clipboard_text(&mut self) -> Result<Option<String>, PluginError> {
        // An SDK call cannot use process-local native state from an arbitrary thread.
        let _mtm = main_thread()?;
        require_generation(self.generation)?;
        autoreleasepool(|_| {
            // The returned String is the explicit call result, never a resident clipboard cache.
            let pasteboard = super::application_pasteboard();
            // SAFETY: AppKit exports this immutable type constant for the process lifetime.
            let text_type = unsafe { NSPasteboardTypeString };
            Ok(pasteboard
                .stringForType(text_type)
                .map(|value| value.to_string()))
        })
    }

    /// Submit one explicit pasteboard replacement while retaining no plugin body after return.
    fn write_clipboard_text(&mut self, text: &str) -> Result<(), PluginError> {
        // No session borrow is held while the foreign pasteboard implementation executes.
        let _mtm = main_thread()?;
        require_generation(self.generation)?;
        autoreleasepool(|_| {
            let pasteboard = super::application_pasteboard();
            pasteboard.clearContents();
            // SAFETY: the public immutable AppKit pasteboard type is valid for this call.
            let text_type = unsafe { NSPasteboardTypeString };
            // Release the native conversion at the pool boundary, propagating write rejection.
            pasteboard
                .setString_forType(&NSString::from_str(text), text_type)
                .then_some(())
                .ok_or(PluginError::new(
                    "clipboard_write_failed",
                    "系统剪贴板拒绝写入",
                ))
        })
    }

    /// Share only the generation-scoped callback contract, never the session resource graph.
    fn foreground_lifecycle(&self) -> Rc<dyn PluginHostLifecycle> {
        // The session retains its callback contract, not the temporary activation context.
        Rc::clone(&self.lifecycle)
    }
}

/// Names the static command whose resource graph participates in foreground transitions.
struct Lifecycle {
    target: PluginCommandTarget,
    generation: u64,
}

impl PluginHostLifecycle for Lifecycle {
    /// Commit this exact prepared activation after native setup succeeds.
    fn commit_creation(&self) -> Result<(), PluginError> {
        // Static descriptors may repeat across sessions; only this activation epoch owns the claim.
        require_generation(self.generation)?;
        // The native adapter prepares its controls before it becomes the foreground owner.
        super::FOREGROUND_STATE.with_borrow(|state| {
            state
                .complete_creation(ResumeTarget::Plugin(self.target))
                .map(|_| ())
                .map_err(|_| {
                    PluginError::new("foreground_creation_failed", "前台插件创建状态不一致")
                })
        })
    }

    /// Accept one destruction direction for the current activation before native close begins.
    fn begin_close(&self, request: PluginCloseRequest) -> Result<(), PluginError> {
        // A retained callback from a previous instance cannot close the same plugin's new session.
        require_generation(self.generation)?;
        validate_terminal_ticket(request)?;
        validate_close_request(
            ACCEPTED_CLOSE.get(),
            request,
            super::termination::application_termination_pending(),
        )?;
        if CLOSE_IN_FLIGHT
            .get()
            .is_some_and(|expected| expected != request)
        {
            // The plugin must acknowledge the exact Host-lent close request, without replacing scope or reversing successor intent.
            return Err(PluginError::new(
                "close_request_mismatch",
                "插件确认的关闭请求与当前请求不一致",
            ));
        }
        let already_accepted = ACCEPTED_CLOSE.get().is_some();
        // Only this target can claim close; no native effects happen inside the state borrow.
        super::FOREGROUND_STATE.with_borrow(|state| {
            if already_accepted {
                // A monotonic promotion or exact duplicate retains its original Destroying transaction;
                // begin_destruction deliberately rejects a second admission, so validate without re-beginning it.
                if state.active_surface() == Some(ForegroundSurface::Plugin(self.target))
                    && state.pending_transition()
                        == Some(crate::SurfaceTransition::Destroying {
                            surface: ForegroundSurface::Plugin(self.target),
                            restore_launcher: request.intent == PluginCloseIntent::ExitToLauncher,
                        })
                {
                    Ok(())
                } else {
                    // Acceptance cannot hide lost foreground ownership; reject further progress and preserve the current closing state.
                    Err(PluginError::new(
                        "foreground_close_failed",
                        "已接受的插件关闭请求不再拥有当前前台交接",
                    ))
                }
            } else {
                // Only the first accepted request reserves the exact underlying foreground destruction.
                state
                    .begin_destruction(
                        ForegroundSurface::Plugin(self.target),
                        host_intent(request.intent),
                    )
                    .map(|_| ())
                    .map_err(|_| {
                        PluginError::new("foreground_close_failed", "前台插件关闭状态不一致")
                    })
            }
        })?;
        // Publish only after the pure intent claim succeeds; monotonic promotion never changes its original successor.
        ACCEPTED_CLOSE.set(Some(request));
        if request.scope == PluginCloseScope::ApplicationTermination {
            // Record only the actual coordinator ticket that authorized this accepted monotonic scope.
            ACCEPTED_TERMINATION.set(super::termination::application_termination_ticket());
        }
        Ok(())
    }

    /// Complete only the accepted direction after native teardown and any borrowed method invocation.
    fn finish_close(&self, request: PluginCloseRequest) {
        if OWNER_GENERATION.get() != Some(self.generation) {
            // A late callback has no authority over a newer activation, even with the same command.
            return;
        }
        if !completion_matches_transition(self.generation, self.target, request) {
            // A callback without the exact accepted close intent cannot discard a live session or
            // consume another transition, including a synchronous callback on its own method stack.
            return;
        }
        if CALL_IN_FLIGHT.get() {
            // A synchronous plugin callback cannot destroy its own borrowed trait object.
            if DEFERRED_FINISH.get().is_some() {
                // Preserve the first accepted completion; duplicate/opposing callbacks cannot replace it.
                return;
            }
            DEFERRED_FINISH.set(Some((self.generation, self.target, request)));
            return;
        }
        // Native ownership has ended and no session method is currently borrowed.
        finish(self.generation, self.target, request);
    }
}

/// Report active or locally invoked ownership without exposing the session itself.
pub(super) fn exists() -> bool {
    // The foreground guard remains true while a close call temporarily owns the Box locally.
    CALL_IN_FLIGHT.get()
        || FINALIZATION_IN_FLIGHT.get()
        || SESSION.with_borrow(|session| session.is_some())
}

/// An unaccepted local plugin invocation must not be interrupted by an application-termination preflight.
pub(super) fn is_call_in_flight() -> bool {
    // Nested terminate must not enter a modal wait that prevents this original SDK/Drop stack from returning.
    CALL_IN_FLIGHT.get() || FINALIZATION_IN_FLIGHT.get()
}

/// Request an ordinary foreground transition with its complete native-graph release requirement.
pub(super) fn request_close(intent: PluginCloseIntent) -> Result<(), PluginError> {
    // This explicit Host convenience maps a known foreground action, without accepting an older SDK/protocol surface.
    request_close_scoped(PluginCloseRequest {
        intent,
        scope: PluginCloseScope::ForegroundTransition,
    })
}

/// Promote or initiate terminal resource drain while retaining any already accepted successor intent.
pub(super) fn request_application_termination() -> Result<(), PluginError> {
    // Existing close intent is immutable; terminal scope suppresses its successor rather than replacing that intent.
    let intent = ACCEPTED_CLOSE
        .get()
        .map_or(PluginCloseIntent::DismissAndResume, |request| {
            // Reuse the exact intent accepted before Quit, including ExitToLauncher during a pending ordinary close.
            request.intent
        });
    request_close_scoped(PluginCloseRequest {
        intent,
        scope: PluginCloseScope::ApplicationTermination,
    })
}

/// Invoke the exact SDK request without holding a RefCell borrow across arbitrary plugin/native code.
fn request_close_scoped(request: PluginCloseRequest) -> Result<(), PluginError> {
    validate_terminal_ticket(request)?;
    validate_close_request(
        ACCEPTED_CLOSE.get(),
        request,
        super::termination::application_termination_pending(),
    )?;
    if CALL_IN_FLIGHT.get() {
        return if CLOSE_IN_FLIGHT.get() == Some(request) {
            // The first invocation owns this same close direction, including eventual save failure.
            Ok(())
        } else {
            // An opposing recursive request, or a request during activation, cannot claim success
            // for an intent the currently borrowed plugin method has never accepted.
            Err(PluginError::new(
                "close_in_progress",
                "Another plugin operation is in progress",
            ))
        };
    }
    let mut session = SESSION
        .take()
        .ok_or(PluginError::new("session_missing", "当前没有活动插件会话"))?;
    // Keep the Box locally alive across callbacks while making invocation/intent ownership explicit.
    CALL_IN_FLIGHT.set(true);
    CLOSE_IN_FLIGHT.set(Some(request));
    let result = session.request_close(request);
    CLOSE_IN_FLIGHT.set(None);
    CALL_IN_FLIGHT.set(false);
    // The trait-object method has returned. Restore the unique Box before any deferred completion;
    // the central validated finalizer alone may invalidate the view and destroy session resources.
    SESSION.with_borrow_mut(|active| *active = Some(session));
    if let Some((generation, target, completed_request)) = DEFERRED_FINISH.take() {
        // Completion revalidates the exact pending transition before removing any host ownership.
        finish(generation, target, completed_request);
    }
    result
}

/// Activate only a validated static registration; this path knows no concrete plugin identity.
pub(super) fn activate(
    registration: PluginRegistration,
    command_id: &'static str,
) -> Result<(), PluginError> {
    // Application termination excludes new plugin owners, including queued actions accepted before Quit.
    if super::termination::suppress_successors() {
        // Preserve the original pending close and allocate no successor session or native view.
        return Err(PluginError::new(
            "application_terminating",
            "Application termination is in progress",
        ));
    }
    // Main-thread proof is required before a plugin may receive native host capabilities.
    let _mtm = main_thread()?;
    let target = PluginCommandTarget {
        registration,
        command_id,
    };
    // Reserve a fresh epoch before any state/resource claim; exhaustion is explicit, never wrapped.
    let generation = LAST_GENERATION
        .get()
        .checked_add(1)
        .ok_or(PluginError::new(
            "session_generation_exhausted",
            "Plugin activation generation exhausted",
        ))?;
    LAST_GENERATION.set(generation);
    super::FOREGROUND_STATE
        .with_borrow(|state| state.begin_creation_for(ResumeTarget::Plugin(target)))
        .map_err(|_| PluginError::new("foreground_creation_failed", "前台插件创建状态不一致"))?;
    // Publish epoch authority before arbitrary plugin code can call lifecycle/mount capabilities.
    OWNER_GENERATION.set(Some(generation));
    // No close scope from an expired session can authorize this new owner, even for the same static target.
    ACCEPTED_CLOSE.set(None);
    ACCEPTED_TERMINATION.set(None);
    // Native creation may reenter lifecycle callbacks before its handler returns.
    CALL_IN_FLIGHT.set(true);
    let activation = (|| {
        // Resolve paths lazily; descriptor discovery never creates a vault or plugin state.
        let storage = super::native_plugin_storage_locations()
            .map_err(|_| PluginError::new("storage_unavailable", "插件存储目录不可用"))?;
        let mut host = HostContext {
            generation,
            storage,
            lifecycle: Rc::new(Lifecycle { target, generation }),
        };
        let invocation = PluginInvocation {
            command_id,
            query: "",
            selected_text: None,
            selected_path: None,
            modifiers: ModifierFlags::default(),
        };
        // End construction/mount/show temporary native claims within this activation, independently of the Launcher
        // mouse-tracking or AX caller's autorelease pool. The returned owned session and its formal Retained graph
        // outlive this local pool. No TLS/RefCell borrow crosses draining; CALL_IN_FLIGHT still guards reentry and
        // DEFERRED_FINISH coalesces it. Pool drain alone proves no deallocation and cannot authorize a successor.
        autoreleasepool(|_| registration.activate(&mut host, &invocation))
    })();
    CALL_IN_FLIGHT.set(false);
    match activation {
        // Inline completion transfers no session and must release its own temporary creation claim.
        Ok(PluginActivation::Completed) => {
            if let Some((completed_generation, completed_target, completed_request)) =
                DEFERRED_FINISH.take()
            {
                // A viewless inline operation may have completed an explicit lifecycle.
                finish(completed_generation, completed_target, completed_request);
            } else {
                // Ordinary inline work transferred no resources or foreground owner.
                cancel_creation(generation, target);
            }
            Ok(())
        }
        // A session transfers unique ownership only after its exact foreground creation was committed.
        Ok(PluginActivation::Session(session)) => {
            if exists() {
                // Do not replace an existing owner even if a plugin violates creation ordering.
                cancel_creation(generation, target);
                return Err(PluginError::new(
                    "session_already_active",
                    "宿主已有活动插件会话",
                ));
            }
            if !super::FOREGROUND_STATE.with_borrow(|state| {
                state.active_surface() == Some(ForegroundSurface::Plugin(target))
            }) {
                // Returning a session without committing its matching creation cannot manufacture
                // foreground ownership. Drop acquired resources before releasing the creation claim.
                drop(session);
                cancel_creation(generation, target);
                return Err(PluginError::new(
                    "foreground_not_committed",
                    "Plugin session did not commit its foreground ownership",
                ));
            }
            // The handler prepared/committed its native graph and transfers the single owner.
            SESSION.with_borrow_mut(|active| *active = Some(session));
            if let Some((completed_generation, completed_target, completed_request)) =
                DEFERRED_FINISH.take()
            {
                // A native close during activation releases the returned session only
                // after the activation handler has stopped borrowing its own resources.
                finish(completed_generation, completed_target, completed_request);
            }
            Ok(())
        }
        // Plugin errors retain their stable SDK cause while partial activation authority is unwound.
        Err(error) => {
            if let Some((completed_generation, completed_target, completed_request)) =
                DEFERRED_FINISH.take()
            {
                // Completed native cleanup remains authoritative even if activation reports failure.
                finish(completed_generation, completed_target, completed_request);
            } else {
                // The handler owns partial native rollback; the host releases its claim/borrow.
                cancel_creation(generation, target);
            }
            Err(error)
        }
    }
}

/// Remove a failed activation's temporary identity and borrowed mount.
fn cancel_creation(generation: u64, target: PluginCommandTarget) {
    if OWNER_GENERATION.get() != Some(generation) {
        // Failed work from a stale activation cannot invalidate a later borrowed view or claim.
        return;
    }
    OWNER_GENERATION.set(None);
    ACCEPTED_CLOSE.set(None);
    ACCEPTED_TERMINATION.set(None);
    // Never leave an invalid copied native handle after construction failed.
    MOUNTED_VIEW.set(None);
    let _ = super::FOREGROUND_STATE
        .with_borrow(|state| state.cancel_creation(ResumeTarget::Plugin(target)));
}

/// Marks the nonreentrant session-Drop/barrier section without retaining a session or native object.
struct PluginFinalizationScope;

impl Drop for PluginFinalizationScope {
    fn drop(&mut self) {
        // Every return/unwind clears only this main-thread stack marker; no native effects occur in the guard.
        FINALIZATION_IN_FLIGHT.set(false);
    }
}

/// Release session resources before consuming the project barrier or restoring Launcher.
fn finish(generation: u64, target: PluginCommandTarget, request: PluginCloseRequest) {
    if OWNER_GENERATION.get() != Some(generation) {
        // Validate dynamic owner identity before the static command/lifecycle state can be inspected.
        return;
    }
    if !completion_matches_transition(generation, target, request) {
        // Preserve the owner, borrowed mount, and original successor when completion has no matching
        // accepted close; a plugin cannot destroy its live session by reporting the wrong direction.
        return;
    }
    // A plugin destructor may reenter AppKit. Veto nested quit until this exact Drop/barrier stack can return.
    FINALIZATION_IN_FLIGHT.set(true);
    let finalization = PluginFinalizationScope;
    // Native teardown precedes this callback; validate identity before touching host ownership.
    let completed = super::FOREGROUND_STATE
        .with_borrow(|state| state.complete_destruction(ForegroundSurface::Plugin(target)));
    let restore = match completed {
        Ok(restore) => {
            // The matched transition records the successor selected when close began.
            restore
        }
        Err(error) => {
            // Duplicate/stale callbacks must not release a newer session or mounted borrow.
            eprintln!("native plugin teardown transition rejected: {error}");
            return;
        }
    };
    // Stop host access before dropping the plugin's remaining core/session resources.
    MOUNTED_VIEW.set(None);
    OWNER_GENERATION.set(None);
    // Exact request authority ends before arbitrary session Drop can reenter stale lifecycle callbacks.
    ACCEPTED_CLOSE.set(None);
    ACCEPTED_TERMINATION.set(None);
    // Invalidate epoch authority before Drop may reenter retained old lifecycle callbacks.
    let released_session = SESSION.take();
    drop(released_session);
    let acknowledged = super::FOREGROUND_STATE
        .with_borrow(|state| state.consume_destroyed_barrier(ForegroundSurface::Plugin(target)));
    if let Err(error) = acknowledged {
        // An inconsistent barrier cannot authorize creation of another business surface.
        eprintln!("native plugin teardown acknowledgement rejected: {error}");
        return;
    }
    if restore != (request.intent == PluginCloseIntent::ExitToLauncher) {
        // Preserve zero surfaces instead of guessing an inconsistent next owner.
        eprintln!("native plugin close intent disagrees with foreground ownership");
        return;
    }
    #[cfg(feature = "native-acceptance")]
    if request.scope == PluginCloseScope::ApplicationTermination {
        // Record the exact validated terminal SDK receipt only after session Drop and barrier consumption.
        super::acceptance_termination::record_terminal_release();
    }
    #[cfg(feature = "native-product-acceptance")]
    {
        // Notify only after matching native receipt, session Drop and Host barrier; this does not create the next Launcher.
        super::acceptance_product::plugin_released(generation, target, request);
    }
    // Actual session Drop and barrier acknowledgement ended; release the stack gate before signalling app quit.
    drop(finalization);
    if super::termination::foreground_did_release() {
        // Actual plugin/native release, session Drop and the consumed barrier now serve the pending app quit.
        return;
    }
    if restore && let Some(mtm) = MainThreadMarker::new() {
        // All state borrows and resource owners have ended before native creation resumes.
        super::show_launcher_surface(mtm);
    }
}

/// The explicit product probe copies the current dynamic session generation without owning plugin or mounted views.
#[cfg(feature = "native-product-acceptance")]
pub(super) fn product_probe_generation() -> Option<u64> {
    OWNER_GENERATION.get()
}

/// Copy only actual accepted close matching Host Destroying; failed cleanup cannot wait on an expected phase alone.
#[cfg(feature = "native-product-acceptance")]
pub(super) fn product_probe_accepted_close()
-> Option<(u64, PluginCommandTarget, PluginCloseRequest)> {
    let generation = OWNER_GENERATION.get()?;
    let request = ACCEPTED_CLOSE.get()?;
    let target = super::FOREGROUND_STATE.with_borrow(|state| {
        if let Some(ForegroundSurface::Plugin(target)) = state.active_surface() {
            // Copy only the active static target; the surrounding generation/request guard proves this activation.
            Some(target)
        } else {
            None // An empty foreground or nonplugin owner has no plugin-close successor authority.
        }
    })?;
    // Reuse the production completion guard, including accepted intent/scope and matching Destroying authority.
    completion_matches_transition(generation, target, request)
        .then_some((generation, target, request))
}

/// Check callback epoch, resource identity, and accepted successor before any completion side effect.
fn completion_matches_transition(
    generation: u64,
    target: PluginCommandTarget,
    request: PluginCloseRequest,
) -> bool {
    if OWNER_GENERATION.get() != Some(generation) {
        // An expired epoch cannot gain authority merely because its static command is reused.
        return false;
    }
    if ACCEPTED_CLOSE.get() != Some(request) {
        // Missing acceptance, stale pre-promotion receipt and a mismatched scope never authorize resource release.
        return false;
    }
    if request.scope == PluginCloseScope::ApplicationTermination
        && (ACCEPTED_TERMINATION.get().is_none()
            || ACCEPTED_TERMINATION.get() != super::termination::application_termination_ticket())
    {
        // An empty framework-window lease is terminal-only evidence; it can never authorize an ordinary successor.
        return false;
    }
    super::FOREGROUND_STATE.with_borrow(|state| {
        // Copy/compare only pure lifecycle values; no plugin or native callback runs under this borrow.
        state.active_surface() == Some(ForegroundSurface::Plugin(target))
            && state.pending_transition()
                == Some(crate::SurfaceTransition::Destroying {
                    surface: ForegroundSurface::Plugin(target),
                    restore_launcher: request.intent == PluginCloseIntent::ExitToLauncher,
                })
    })
}

/// Reject a terminal receipt or repeated admission that belongs to a cancelled earlier delegate transaction.
fn validate_terminal_ticket(request: PluginCloseRequest) -> Result<(), PluginError> {
    if request.scope == PluginCloseScope::ApplicationTermination
        && let Some(accepted) = ACCEPTED_TERMINATION.get()
        && Some(accepted) != super::termination::application_termination_ticket()
    {
        // Matching session generation/request fields cannot transfer a canceled quit's authority to a new transaction.
        return Err(PluginError::new(
            "termination_transaction_mismatch",
            "插件终止关闭属于另一次应用退出事务",
        ));
    }
    Ok(())
}

/// Validate the single accepted request's intent/scope order before invoking or accepting plugin effects.
fn validate_close_request(
    accepted: Option<PluginCloseRequest>,
    requested: PluginCloseRequest,
    application_terminating: bool,
) -> Result<(), PluginError> {
    if requested.scope == PluginCloseScope::ApplicationTermination && !application_terminating {
        // A queued request cannot authorize terminal cleanup; the delegate must own an actual pending termination transaction.
        return Err(PluginError::new(
            "termination_not_pending",
            "应用尚未进入退出流程，无法执行插件终止清理",
        ));
    }
    if let Some(accepted) = accepted {
        if accepted.intent != requested.intent {
            // Promotion to application termination preserves accepted successor intent; opposing intent cannot replace it.
            return Err(PluginError::new(
                "close_in_progress",
                "已有不同意图的关闭请求正在处理",
            ));
        }
        if accepted.scope == PluginCloseScope::ApplicationTermination
            && requested.scope == PluginCloseScope::ForegroundTransition
        {
            // Once cleanup begins, a foreground receipt cannot replace the accepted application-termination receipt.
            return Err(PluginError::new(
                "close_scope_downgrade",
                "已接受的应用终止关闭不能降级为普通前台关闭",
            ));
        }
    }
    Ok(())
}

/// Reject old capability callbacks before they can inspect or mutate a new session's native state.
fn require_generation(generation: u64) -> Result<(), PluginError> {
    if OWNER_GENERATION.get() != Some(generation) {
        // Reactivating the static command does not revive old-session callbacks or permit access to the new session.
        return Err(PluginError::new("stale_session", "插件会话已失效"));
    }
    Ok(())
}

/// Translate equivalent SDK/application intents at their explicit contract boundary.
const fn host_intent(intent: PluginCloseIntent) -> SurfaceCloseIntent {
    match intent {
        // Temporary dismissal retains only the static command as a resume identity.
        PluginCloseIntent::DismissAndResume => SurfaceCloseIntent::DismissAndResume,
        // Explicit exit restores Launcher after complete resource release.
        PluginCloseIntent::ExitToLauncher => SurfaceCloseIntent::ExitToLauncher,
    }
}

/// Establish platform authority at each SDK-facing native entry point.
fn main_thread() -> Result<MainThreadMarker, PluginError> {
    // A background caller receives a typed failure without accessing thread-local native state.
    MainThreadMarker::new().ok_or(PluginError::new(
        "main_thread_unavailable",
        "AppKit 主线程不可用",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a typed ordinary close request for the actual pure admission policy.
    fn ordinary(intent: PluginCloseIntent) -> PluginCloseRequest {
        PluginCloseRequest {
            intent,
            scope: PluginCloseScope::ForegroundTransition,
        }
    }

    /// Request admission preserves the complete intent/scope order and requires actual quit authority.
    #[test]
    fn terminal_scope_requires_pending_quit_and_never_changes_intent_or_downgrades() {
        for intent in [
            PluginCloseIntent::DismissAndResume,
            PluginCloseIntent::ExitToLauncher,
        ] {
            // Both successor directions admit the same single monotonic scope promotion.
            let foreground = ordinary(intent);
            let terminal = PluginCloseRequest {
                scope: PluginCloseScope::ApplicationTermination,
                ..foreground
            };
            assert_eq!(
                validate_close_request(None, terminal, false)
                    .unwrap_err()
                    .code(),
                "termination_not_pending"
            );
            assert!(validate_close_request(Some(foreground), terminal, true).is_ok());
            assert!(validate_close_request(Some(terminal), terminal, true).is_ok());
            assert_eq!(
                validate_close_request(Some(terminal), foreground, true)
                    .unwrap_err()
                    .code(),
                "close_scope_downgrade"
            );
            let other = PluginCloseRequest {
                intent: match intent {
                    // Only destination identity changes in these adversarial requests; scope cannot hide that change.
                    PluginCloseIntent::DismissAndResume => PluginCloseIntent::ExitToLauncher,
                    PluginCloseIntent::ExitToLauncher => PluginCloseIntent::DismissAndResume,
                },
                ..terminal
            };
            assert_eq!(
                validate_close_request(Some(foreground), other, true)
                    .unwrap_err()
                    .code(),
                "close_in_progress"
            );
        }
    }
}
