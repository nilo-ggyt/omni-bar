//! Track explicit construction roles with weak identities; AppKit reachability does not establish ownership.

use super::native_deallocation::NativeDeallocationCohort;
use objc2::MainThreadMarker;
use objc2::rc::Weak;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSControl, NSMenu};
use std::cell::RefCell;
use std::collections::HashMap;

const MAX_NATIVE_PAYLOAD_OBJECTS: usize = 1024;

#[derive(Clone)]
/// Static role names and zeroing weak references retain neither business data nor native owners.
struct OwnedIdentity {
    role: &'static str,
    object: Weak<AnyObject>,
}

#[derive(Clone, Default)]
/// A close receipt contains bounded explicit project identities; its address index is construction-only.
pub(super) struct NativePayloadSnapshot {
    objects: Vec<OwnedIdentity>,
    identities: HashMap<usize, usize>,
}

impl NativePayloadSnapshot {
    /// Record a role the caller actually created or configured, rather than inferring ownership from its class.
    pub(super) fn include(&mut self, role: &'static str, object: &AnyObject) -> Result<(), String> {
        let identity = std::ptr::from_ref(object).addr();
        if self.contains_live_identity(identity) {
            // Multiple roles for one live identity require only one actual deallocation event.
            return Ok(());
        }
        if self.objects.len() == MAX_NATIVE_PAYLOAD_OBJECTS {
            // Menu rebuilds replace items; reclaim only nil entries and preserve evidence of surviving old items.
            self.objects.retain(|entry| entry.object.load().is_some());
            self.identities.clear();
            for (index, entry) in self.objects.iter().enumerate() {
                if let Some(object) = entry.object.load() {
                    // Temporary main-thread weak-load claims end before any AppKit message or event-loop turn.
                    self.identities
                        .insert(std::ptr::from_ref(&*object).addr(), index);
                }
            }
        }
        if self.objects.len() == MAX_NATIVE_PAYLOAD_OBJECTS {
            // Overflow fails explicitly; omitting roles cannot authorize a successor or process exit.
            return Err("原生资源登记数量超出固定上限".into());
        }
        self.identities.insert(identity, self.objects.len());
        self.objects.push(OwnedIdentity {
            role,
            object: Weak::new(object),
        });
        Ok(())
    }

    /// Seal role capture for close and release the address index used only during construction.
    pub(super) fn seal(&mut self) {
        self.identities = HashMap::new();
    }

    /// Register a replacement at a reused address when the old weak identity is already nil.
    fn contains_live_identity(&self, identity: usize) -> bool {
        self.identities.get(&identity).is_some_and(|index| {
            // Only a still-live old identity proves that this address is already recorded.
            self.objects[*index].object.load().is_some()
        })
    }

    /// Observe deallocation only for surviving explicit roles, not for borrowed framework caches.
    pub(super) fn track_remaining(
        &self,
        cohort: &mut NativeDeallocationCohort,
        mtm: MainThreadMarker,
    ) {
        for entry in &self.objects {
            if let Some(object) = entry.object.load() {
                // This temporary strong claim ends in the iteration; the cohort callback never owns the observed object.
                cohort.track(&*object, mtm);
            }
        }
    }

    /// Require every explicit role to be nil; parent-view or controller release is insufficient evidence.
    pub(super) fn is_released(&self) -> bool {
        self.objects.iter().all(|entry| {
            // Return a scalar result only; no temporary native claim escapes this closure.
            entry.object.load().is_none()
        })
    }

    /// Report bounded static role names without reading user text or private class names.
    pub(super) fn surviving_roles(&self) -> Vec<&'static str> {
        self.objects
            .iter()
            .filter_map(|entry| {
                // Diagnostics grant no authority; any surviving role still fails the release condition.
                entry.object.load().map(|_| entry.role)
            })
            .collect()
    }
}

#[derive(Default)]
/// Construction ledger; complete potentially reentrant AppKit getters before borrowing its state.
pub(super) struct NativeOwnedLedger {
    snapshot: RefCell<NativePayloadSnapshot>,
}

impl NativeOwnedLedger {
    /// Record runtime weak identities without sending AppKit messages.
    pub(super) fn include(&self, role: &'static str, object: &AnyObject) -> Result<(), String> {
        self.snapshot.borrow_mut().include(role, object)
    }

    /// Record a configured control and its actual cell without traversing framework-owned descendants.
    pub(super) fn include_control(
        &self,
        role: &'static str,
        control: &NSControl,
    ) -> Result<(), String> {
        let cell = control.cell();
        self.include(role, control)?;
        if let Some(cell) = cell {
            // The configured cell carries project text and actions, so its release needs independent evidence.
            self.include(role, &cell)?;
        }
        Ok(())
    }

    /// Record configured menus and items while preserving evidence of surviving replaced items.
    pub(super) fn include_menu(&self, role: &'static str, menu: &NSMenu) -> Result<(), String> {
        let items = menu.itemArray();
        if items.count() > MAX_NATIVE_PAYLOAD_OBJECTS {
            // An oversized native array cannot be certified by silently truncating the role inventory.
            return Err("原生菜单项目数量超出固定上限".into());
        }
        self.include(role, menu)?;
        for index in 0..items.count() {
            // Obtain each native item before borrowing the ledger so getter reentry cannot encounter its RefMut.
            let item = items.objectAtIndex(index);
            self.include(role, &item)?;
        }
        Ok(())
    }

    /// Copy bounded weak metadata without adding strong claims to observed native objects.
    pub(super) fn snapshot(&self) -> NativePayloadSnapshot {
        self.snapshot.borrow().clone()
    }
}

/// A dedicated native child checks explicit roles, address reuse and unregistered descendant ownership.
#[cfg(feature = "native-acceptance")]
pub(super) fn verify_native_payload_accounting(mtm: MainThreadMarker) -> Result<(), String> {
    use objc2::MainThreadOnly;
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSObject, NSRect};
    let mut snapshot = NativePayloadSnapshot::default();
    let original = NSObject::new();
    snapshot.include("test.original", &original)?;
    drop(original);
    let replacement = NSObject::new();
    // Force an index collision with an old nil slot while retaining a genuinely different replacement object.
    snapshot
        .identities
        .insert(std::ptr::from_ref(&*replacement).addr(), 0);
    snapshot.include("test.replacement", &replacement)?;
    if snapshot.objects.len() != 2 || snapshot.is_released() {
        // A stale address index must not hide the current live replacement.
        return Err("Native owned ledger address-reuse accounting failed".into());
    }
    drop(replacement);
    if !snapshot.is_released() {
        // Weak metadata must not extend the lifetime of a native resource.
        return Err("Native owned ledger retained an observed object".into());
    }
    let snapshot = autoreleasepool(|_| {
        let ledger = NativeOwnedLedger::default();
        let root = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        let child = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        root.addSubview(&child);
        ledger.include("test.root", &root)?;
        if ledger.snapshot().objects.len() != 1 {
            // Reachability alone must not classify an unregistered child as an explicitly owned project object.
            return Err("Native owned ledger inferred ownership from a subtree".to_owned());
        }
        ledger.include("test.child", &child)?;
        ledger.include("test.child-alias", &child)?;
        let mut snapshot = ledger.snapshot();
        if snapshot.objects.len() != 2 || snapshot.surviving_roles().len() != 2 {
            // Include the new role, but observe aliases of the same live identity only once.
            return Err("Native owned ledger explicit-role accounting failed".to_owned());
        }
        snapshot.seal();
        Ok(snapshot)
    })?;
    if !snapshot.is_released() {
        // Local owners and their autorelease pool have ended, so all recorded weak identities must be nil.
        return Err("Native owned ledger retained construction roles".into());
    }
    Ok(())
}
