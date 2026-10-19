//! Native control construction and symmetric callback detachment for one built-in surface.

use objc2::rc::Retained;
use objc2::runtime::{ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSBackingStoreType, NSButton, NSColor, NSFont, NSPopUpButton,
    NSScrollView, NSTextAlignment, NSTextField, NSUserInterfaceItemIdentification, NSView,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::super::native_payload::NativeOwnedLedger;
use super::{BuiltInController, BuiltInPanel};
use crate::BuiltInSurface;
use crate::capabilities::clipboard::{
    ClipboardEntry, MAX_ENTRY_COUNT, MAX_QUERY_UTF16, clipboard_query_prefix,
};
use crate::capabilities::{shortcut_preferences::ShortcutBinding, unit_conversion};

const WIDTH: f64 = 760.0;
const HEIGHT: f64 = 500.0;

define_class!(
    /// A foreground-only fixed-row document view places newest clipboard previews at its top edge.
    // SAFETY: a flipped NSView changes only its public coordinate-system convention;
    // it owns no extra ivars and remains confined to the AppKit main thread.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    struct ClipboardList;

    impl ClipboardList {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            // The newest preview begins at the top of a freshly displayed scroll document.
            true
        }
    }
);

/// Explicit session-held controls; menu/control target pointers never own the delegate.
pub(super) struct BuiltInViews {
    pub(super) input: Retained<NSTextField>,
    pub(super) output: Retained<NSTextField>,
    pub(super) from: Retained<NSPopUpButton>,
    pub(super) to: Retained<NSPopUpButton>,
    pub(super) shortcuts: Retained<NSPopUpButton>,
    pub(super) rows: Vec<Retained<NSButton>>,
    actions: Vec<Retained<NSButton>>,
}

impl BuiltInViews {
    /// Register replacement menu roles while retaining only weak evidence of surviving old items.
    pub(super) fn register_menus(&self, ledger: &NativeOwnedLedger) -> Result<(), String> {
        for (role, popup) in [
            ("units.from-menu", &self.from),
            ("units.to-menu", &self.to),
            ("settings.shortcuts-menu", &self.shortcuts),
        ] {
            if popup.numberOfItems() > 0 {
                // Register only configured menus; internal objects of an unused empty picker remain framework-owned.
                let menu = popup
                    .menu()
                    .ok_or_else(|| "已配置的内建选择器没有菜单".to_owned())?;
                ledger.include_menu(role, &menu)?;
            }
        }
        Ok(())
    }

    /// Remove every target/delegate before releasing the session's retained callback owner.
    pub(super) fn detach(&self) {
        // SAFETY: these are live session-owned controls; clearing target/action prevents
        // any later native delivery to the soon-to-be-released controller.
        unsafe {
            self.input.setDelegate(None);
            self.input.setTarget(None);
            self.input.setAction(None);
            self.from.setTarget(None);
            self.from.setAction(None);
            self.to.setTarget(None);
            self.to.setAction(None);
            self.shortcuts.setTarget(None);
            self.shortcuts.setAction(None);
        }
        for button in self.actions.iter().chain(&self.rows) {
            // SAFETY: each retained button is live and clearing a nullable target/action is valid.
            unsafe {
                button.setTarget(None);
                button.setAction(None);
            }
        }
    }
}

/// Construct a configured panel and only the controls required by its concrete capability.
pub(super) fn build(
    kind: BuiltInSurface,
    controller: &BuiltInController,
    mtm: MainThreadMarker,
    ledger: &NativeOwnedLedger,
) -> Result<(Retained<BuiltInPanel>, BuiltInViews), String> {
    // SAFETY: this NSPanel subclass has no ivars; initialization uses the exact AppKit
    // designated initializer and is performed on its allocating main thread.
    let panel: Retained<BuiltInPanel> = unsafe {
        msg_send![
            super(BuiltInPanel::alloc(mtm).set_ivars(())),
            initWithContentRect: rect(0.0, 0.0, WIDTH, HEIGHT),
            styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    // SAFETY: the Rust session owns the allocated panel; AppKit must not independently release it on close.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(&NSString::from_str(title(kind)));
    panel.setDelegate(Some(ProtocolObject::from_ref(controller)));
    panel.setHidesOnDeactivate(false);
    panel.setFloatingPanel(true);
    panel.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, WIDTH, HEIGHT));
    panel.setContentView(Some(&content));
    // Register the explicitly constructed root without claiming AppKit decoration/editor descendants.
    ledger.include("builtin.content", &content)?;

    // Shared navigation and status controls retain bounded text only while this surface exists.
    let back = button(
        "builtin.back",
        ledger,
        "‹ 返回启动器",
        sel!(returnToLauncher:),
        controller,
        mtm,
        rect(18.0, 452.0, 150.0, 30.0),
    )?;
    content.addSubview(&back);
    let output = label(
        "builtin.output",
        ledger,
        "",
        mtm,
        rect(24.0, 15.0, 712.0, 42.0),
    )?;
    output.setSelectable(true);
    output.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    content.addSubview(&output);
    let input = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
    input.setFrame(rect(24.0, 391.0, 712.0, 34.0));
    input.setFont(Some(&NSFont::systemFontOfSize(17.0)));
    // SAFETY: the main-thread session retains this conforming editing delegate until detach;
    // no borrowed control or controller pointer can escape the foreground graph.
    unsafe { input.setDelegate(Some(ProtocolObject::from_ref(controller))) };
    content.addSubview(&input);
    // Input and its actual cell independently carry project text/callbacks and both enter the release ledger.
    ledger.include_control("builtin.input", &input)?;

    // Inactive picker objects stay empty and hidden; only one capability's catalog is populated.
    let from = popup(
        "units.from",
        ledger,
        controller,
        sel!(unitsChanged:),
        mtm,
        rect(24.0, 330.0, 290.0, 30.0),
    )?;
    let to = popup(
        "units.to",
        ledger,
        controller,
        sel!(unitsChanged:),
        mtm,
        rect(360.0, 330.0, 290.0, 30.0),
    )?;
    let shortcuts = popup(
        "settings.shortcuts",
        ledger,
        controller,
        sel!(changeShortcut:),
        mtm,
        rect(24.0, 340.0, 440.0, 32.0),
    )?;
    from.setHidden(true);
    to.setHidden(true);
    shortcuts.setHidden(true);
    content.addSubview(&from);
    content.addSubview(&to);
    content.addSubview(&shortcuts);
    let mut actions = vec![back];
    let mut rows = Vec::new();

    match kind {
        // Calculator results update synchronously through the bounded pure grammar.
        BuiltInSurface::Calculator => {
            input.setPlaceholderString(Some(&NSString::from_str("例如：(12 + 8) * 3")));
            // Preserve the baseline's explicit input identity for native accessibility and focused probe inspection.
            input.setAccessibilityLabel(Some(&NSString::from_str("数学表达式")));
            input.setIdentifier(Some(&NSString::from_str("calculator-expression")));
            output.setAccessibilityLabel(Some(&NSString::from_str("计算结果")));
            output.setIdentifier(Some(&NSString::from_str("calculator-result")));
            content.addSubview(&*label(
                "calculator.hint",
                ledger,
                "表达式 · + − * / % ^ · 括号与科学计数法",
                mtm,
                rect(24.0, 355.0, 680.0, 24.0),
            )?);
            let copy = button(
                "calculator.copy",
                ledger,
                "复制结果",
                sel!(copyResult:),
                controller,
                mtm,
                rect(24.0, 298.0, 130.0, 32.0),
            )?;
            content.addSubview(&copy);
            actions.push(copy);
        }
        // Source/destination selectors share static domain definitions rather than an FFI payload.
        BuiltInSurface::UnitConversion => {
            input.setStringValue(&NSString::from_str("1"));
            input.setPlaceholderString(Some(&NSString::from_str("待换算数值")));
            // Explicit labels distinguish source, destination and value without relying on inferred native naming.
            input.setAccessibilityLabel(Some(&NSString::from_str("待换算数值")));
            input.setIdentifier(Some(&NSString::from_str("unit-conversion-value")));
            from.setAccessibilityLabel(Some(&NSString::from_str("源单位")));
            from.setIdentifier(Some(&NSString::from_str("unit-conversion-from")));
            to.setAccessibilityLabel(Some(&NSString::from_str("目标单位")));
            to.setIdentifier(Some(&NSString::from_str("unit-conversion-to")));
            output.setAccessibilityLabel(Some(&NSString::from_str("换算结果")));
            output.setIdentifier(Some(&NSString::from_str("unit-conversion-result")));
            // SAFETY: the retained main-thread controller implements convertUnits: with the declared action ABI;
            // detach clears these nonowning pointers before the session releases the controller.
            unsafe {
                input.setTarget(Some(controller));
                input.setAction(Some(sel!(convertUnits:)));
            }
            if let Some(cell) = input.cell() {
                // Focus changes do not submit; the normal text-field Enter action and button share convertUnits:.
                cell.setSendsActionOnEndEditing(false);
            }
            from.setHidden(false);
            to.setHidden(false);
            content.addSubview(&*label(
                "units.from-label",
                ledger,
                "从",
                mtm,
                rect(24.0, 365.0, 290.0, 20.0),
            )?);
            content.addSubview(&*label(
                "units.to-label",
                ledger,
                "到",
                mtm,
                rect(360.0, 365.0, 290.0, 20.0),
            )?);
            let definitions = unit_conversion::definitions();
            for definition in &definitions {
                // Fifteen static choices cover exactly length, mass and temperature.
                from.addItemWithTitle(&NSString::from_str(&format!(
                    "{} ({})",
                    definition.label, definition.symbol
                )));
            }
            from.selectItemAtIndex(
                definitions
                    .iter()
                    .position(|unit| {
                        // Meter is the initial source independent of localized catalog labels.
                        unit.id == "meter"
                    })
                    .unwrap_or(0) as isize,
            );
            content.addSubview(&*label(
                "units.arrow",
                ledger,
                "→",
                mtm,
                rect(324.0, 331.0, 30.0, 24.0),
            )?);
            let convert = button(
                "units.convert",
                ledger,
                "换算",
                sel!(convertUnits:),
                controller,
                mtm,
                rect(24.0, 275.0, 130.0, 32.0),
            )?;
            convert.setIdentifier(Some(&NSString::from_str("unit-conversion-submit")));
            content.addSubview(&convert);
            actions.push(convert);
            let copy = button(
                "units.copy",
                ledger,
                "复制结果",
                sel!(copyResult:),
                controller,
                mtm,
                rect(170.0, 275.0, 130.0, 32.0),
            )?;
            content.addSubview(&copy);
            actions.push(copy);
        }
        // The visible panel owns only bounded previews; full bodies remain in transient storage maps.
        BuiltInSurface::Clipboard => {
            // The panel reuses ordinary mouse-move delivery for row selection without per-row tracking resources.
            panel.setAcceptsMouseMovedEvents(true);
            input.setPlaceholderString(Some(&NSString::from_str("搜索最近的纯文本记录")));
            let scroll = NSScrollView::initWithFrame(
                NSScrollView::alloc(mtm),
                rect(24.0, 100.0, 712.0, 275.0),
            );
            scroll.setHasVerticalScroller(true);
            // The project creates the scroll view itself; AppKit supplies its internal clip/scroller composition.
            ledger.include("clipboard.scroll", &scroll)?;
            // SAFETY: initialize the allocated NSView subclass through the exact superclass
            // designated initializer after its empty ivar payload has been established.
            let list: Retained<ClipboardList> = unsafe {
                msg_send![super(ClipboardList::alloc(mtm).set_ivars(())),
                    initWithFrame: rect(0.0, 0.0, 690.0, MAX_ENTRY_COUNT as f64 * 42.0)]
            };
            // Register the project list explicitly instead of inferring ownership from the scroll hierarchy.
            ledger.include("clipboard.list", &list)?;
            for index in 0..MAX_ENTRY_COUNT {
                // A fixed control count prevents unbounded per-history allocation as data changes.
                let row = button(
                    "clipboard.row",
                    ledger,
                    "",
                    sel!(restoreClipboard:),
                    controller,
                    mtm,
                    rect(4.0, index as f64 * 42.0 + 3.0, 676.0, 36.0),
                )?;
                row.setTag(index as isize);
                row.setAlignment(NSTextAlignment::Left);
                row.setHidden(true);
                list.addSubview(&row);
                rows.push(row);
            }
            scroll.setDocumentView(Some(&list));
            content.addSubview(&scroll);
            let clear = button(
                "clipboard.clear",
                ledger,
                "清空历史…",
                sel!(clearClipboard:),
                controller,
                mtm,
                rect(24.0, 63.0, 140.0, 30.0),
            )?;
            content.addSubview(&clear);
            actions.push(clear);
            // Explicit refresh preserves the baseline action independently of capture ticks or query edits.
            let refresh = button(
                "clipboard.refresh",
                ledger,
                "刷新",
                sel!(refreshClipboard:),
                controller,
                mtm,
                rect(176.0, 63.0, 100.0, 30.0),
            )?;
            content.addSubview(&refresh);
            actions.push(refresh);
        }
        // Settings delegates all registration/persistence transitions to the host's shortcut adapter.
        BuiltInSurface::Settings => {
            input.setHidden(true);
            shortcuts.setHidden(false);
            for binding in ShortcutBinding::ALL {
                // The picker offers every historical product choice and no unvalidated raw keycode.
                shortcuts.addItemWithTitle(&NSString::from_str(&binding.display_label()));
            }
            content.addSubview(&*label(
                "settings.title",
                ledger,
                "显示或收起 Omni Bar",
                mtm,
                rect(24.0, 388.0, 650.0, 30.0),
            )?);
            content.addSubview(&*label(
                "settings.hint",
                ledger,
                "选择后立即生效；失败时会提示实际生效状态，保存警告请按提示重试。",
                mtm,
                rect(24.0, 285.0, 690.0, 36.0),
            )?);
        }
    }
    if kind == BuiltInSurface::Settings {
        // The visible settings picker owns initial keyboard focus.
        panel.setInitialFirstResponder(Some(&shortcuts));
    } else {
        // Other capabilities start with their visible numeric/search field focused.
        panel.setInitialFirstResponder(Some(&input));
    }
    let views = BuiltInViews {
        input,
        output,
        from,
        to,
        shortcuts,
        rows,
        actions,
    };
    // Register configured menus/items independently; failure drops partial unshown owners normally.
    views.register_menus(ledger)?;
    Ok((panel, views))
}

/// Register this session's actual alert/buttons; only the first affirmative response authorizes clearing.
pub(super) fn confirm_clear(
    mtm: MainThreadMarker,
    ledger: &NativeOwnedLedger,
) -> Result<bool, String> {
    // Internal windows/views remain framework composites; observe the explicit alert and configured buttons separately.
    let alert = NSAlert::new(mtm);
    ledger.include("clipboard.clear-alert", &alert)?;
    alert.setMessageText(&NSString::from_str("清空剪贴板历史？"));
    alert.setInformativeText(&NSString::from_str(
        "将删除所有已记录的纯文本内容，此操作无法撤销。",
    ));
    let clear = alert.addButtonWithTitle(&NSString::from_str("清空历史"));
    ledger.include_control("clipboard.clear-alert-confirm", &clear)?;
    let cancel = alert.addButtonWithTitle(&NSString::from_str("取消"));
    ledger.include_control("clipboard.clear-alert-cancel", &cancel)?;
    // Cancel/close leaves storage unchanged; drop local owners and retain weak evidence of any surviving roles.
    Ok(alert.runModal() == 1000)
}

/// Map built-in identities to native user-facing titles, independent of plugin descriptors.
fn title(kind: BuiltInSurface) -> &'static str {
    match kind {
        // The calculator exposes bounded arithmetic input and result controls.
        BuiltInSurface::Calculator => "计算器",
        // The conversion panel presents the supported dimensional catalog.
        BuiltInSurface::UnitConversion => "单位换算",
        // History belongs to the explicitly opened foreground clipboard session.
        BuiltInSurface::Clipboard => "超级剪切板",
        // Preferences reflect the actual global-shortcut registration owned by the host.
        BuiltInSurface::Settings => "设置",
    }
}

/// Construct a value rectangle without retaining any layout state.
fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    // AppKit owns coordinates by value; all callers use the fixed foreground panel's bounds.
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// Read the baseline's scalar query prefix without materializing the complete native input value.
pub(super) fn clipboard_search_query(input: &NSTextField) -> String {
    let query = input.stringValue();
    let length = query.length().min(MAX_QUERY_UTF16);
    let mut units = [0; MAX_QUERY_UTF16];
    for (index, unit) in units[..length].iter_mut().enumerate() {
        // Public UTF-16 access stays inside the retained string and finite acquisition prefix; it never commits IME.
        *unit = query.characterAtIndex(index);
    }
    clipboard_query_prefix(&units[..length])
}

/// Format bounded preview metadata using the local wall-clock convention without logging its body.
pub(super) fn clipboard_row_label(entry: &ClipboardEntry) -> String {
    // Keep the initialized C time storage live until strftime has consumed its borrowed pointer.
    let seconds: libc::time_t = entry.captured_at_ms.div_euclid(1_000);
    let mut time_info = std::mem::MaybeUninit::<libc::tm>::uninit();
    let mut formatted = [0u8; 32];
    // SAFETY: both pointers reference correctly aligned live values; localtime_r initializes
    // the tm output on success and does not retain either stack pointer after returning.
    let local = unsafe { libc::localtime_r(&seconds, time_info.as_mut_ptr()) };
    let date = if local.is_null() {
        // Invalid/out-of-range timestamps do not prevent restoring the corresponding valid body.
        String::from("时间未知")
    } else {
        // SAFETY: the successful localtime_r call initialized local; strftime writes at most
        // the supplied buffer size and uses the static NUL-terminated format without retaining it.
        let length = unsafe {
            libc::strftime(
                formatted.as_mut_ptr().cast(),
                formatted.len(),
                c"%m-%d %H:%M".as_ptr(),
                local,
            )
        };
        String::from_utf8_lossy(&formatted[..length]).into_owned()
    };
    format!("{}  · {} · {} B", entry.preview, date, entry.byte_count)
}

/// Construct a native label whose ownership is transferred to its parent and session as needed.
fn label(
    role: &'static str,
    ledger: &NativeOwnedLedger,
    text: &str,
    mtm: MainThreadMarker,
    frame: NSRect,
) -> Result<Retained<NSTextField>, String> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(frame);
    label.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    // Configured cells and controls each retain text; failed registration cannot return an untracked control.
    ledger.include_control(role, &label)?;
    Ok(label)
}

/// Create a session-retained button with its non-owning target installed before display.
fn button(
    role: &'static str,
    ledger: &NativeOwnedLedger,
    text: &str,
    action: Sel,
    controller: &BuiltInController,
    mtm: MainThreadMarker,
    frame: NSRect,
) -> Result<Retained<NSButton>, String> {
    // SAFETY: target is a live main-thread NSObject subclass implementing this exact action
    // selector; the owning session retains it until callbacks are explicitly detached.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(text),
            Some(controller),
            Some(action),
            mtm,
        )
    };
    button.setFrame(frame);
    // Observe every constructed action button and actual cell independently from its parent view.
    ledger.include_control(role, &button)?;
    Ok(button)
}

/// Construct an initially empty popup; only its owning capability populates its menu.
fn popup(
    role: &'static str,
    ledger: &NativeOwnedLedger,
    controller: &BuiltInController,
    action: Sel,
    mtm: MainThreadMarker,
    frame: NSRect,
) -> Result<Retained<NSPopUpButton>, String> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, false);
    // SAFETY: the retained controller implements the declared action and remains alive until detach.
    unsafe {
        popup.setTarget(Some(controller));
        popup.setAction(Some(action));
    }
    // Empty initial menus have no business configuration; register_menus records the populated menu/items.
    ledger.include_control(role, &popup)?;
    Ok(popup)
}
