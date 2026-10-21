//! Observe the real built-in graph without replacing its lifecycle, shortcuts, or control owners.

use objc2::DefinedClass;
use objc2::rc::Weak;
use objc2_app_kit::{
    NSResponder, NSTextField, NSTextView, NSView, NSWindow, NSWindowButton, NSWindowStyleMask,
};
use objc2_foundation::{NSRect, NSSize, NSString};

use super::{ACTIVE, BuiltInSurface};

/// Capture only the window created by the production built-in activation. Full UI checks run on
/// the next main-queue turn, after activation callbacks and their temporary retains have returned.
pub(in crate::platform::macos) fn capture_window(
    kind: BuiltInSurface,
) -> Result<Weak<NSWindow>, String> {
    let session = ACTIVE
        .with_borrow(Clone::clone)
        .ok_or("内建窗口激活后没有实际活动会话")?;
    if session.kind != kind || session.closing.get().is_some() {
        return Err("内建窗口观察不属于原激活会话".into());
    }
    let window: &NSWindow = &session.panel;
    Ok(Weak::from(window))
}

/// Read one production activation after its launch callback returned. Only its weak window
/// identity leaves this call; temporary control, editor, and session claims end before Quit.
pub(in crate::platform::macos) fn verify_ui(
    kind: BuiltInSurface,
) -> Result<Weak<NSWindow>, String> {
    // Copy the actual owner first. AppKit methods run outside the active-session TLS borrow,
    // so real editing/resizing callbacks can resolve their original foreground owner normally.
    let session = ACTIVE
        .with_borrow(Clone::clone)
        .ok_or("内建界面验收没有实际活动会话")?;
    if session.kind != kind || session.closing.get().is_some() {
        // Another surface or an accepted close cannot supply geometry or input evidence for this case.
        return Err("内建界面验收的会话身份已经变化".into());
    }
    let panel = &session.panel;
    let forbidden =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    if panel.styleMask().intersects(forbidden)
        || !panel.styleMask().contains(NSWindowStyleMask::Resizable)
    {
        // This protects the original undecorated/resizable interaction rather than accepting a
        // visually plausible replacement window with native close, minimize, or title controls.
        return Err("内建窗口未保留无边框可缩放交互".into());
    }
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        // A framework-provided hidden button is not visible decoration. No visible traffic light
        // may survive merely because its action was disabled or its native title is empty.
        if panel
            .standardWindowButton(button)
            .is_some_and(|button| !button.isHiddenOrHasHiddenAncestor())
        {
            return Err("内建窗口出现了禁止添加的原生关闭或缩放按钮".into());
        }
    }
    let minimum = panel.contentMinSize();
    if minimum.width != 680.0 || minimum.height != 460.0 {
        // The original usable minimum is an interaction contract, not a guessed reduced test size.
        return Err("内建窗口的原最小可用尺寸发生变化".into());
    }
    let content = panel.contentView().ok_or("内建窗口没有实际内容视图")?;
    let original = panel.frame();
    let original_content = content.bounds().size;
    let expected_title = match kind {
        BuiltInSurface::Calculator => "计算器",
        BuiltInSurface::UnitConversion => "单位换算",
        BuiltInSurface::Clipboard => "超级剪切板",
        BuiltInSurface::Settings => "设置",
    };
    // Native autoresizing executes through the existing view masks. Preserve the exact original
    // frame even when a geometry check fails; no alternate layout or private field mutation is used.
    let geometry = (|| {
        for size in [minimum, NSSize::new(1040.0, 720.0), original_content] {
            panel.setContentSize(size);
            content.layoutSubtreeIfNeeded();
            verify_top_level_geometry(&content, expected_title)?;
            if kind == BuiltInSurface::UnitConversion {
                // The conversion choices share a native row container. Top-level containment
                // alone cannot detect its actual selectors colliding with the arrow inside it.
                verify_unit_choices(&session.views.from, &session.views.to, &content)?;
            }
        }
        Ok::<(), String>(())
    })();
    panel.setFrame_display(original, true);
    content.layoutSubtreeIfNeeded();
    geometry?;
    if !panel.isVisible() || !panel.isKeyWindow() {
        // Order-front requests alone do not prove keyboard readiness of this actual visible panel.
        return Err("内建窗口没有实际显示并取得键盘焦点".into());
    }
    if kind == BuiltInSurface::Settings {
        // Settings keeps its original visible picker focus and never creates a hidden text editor
        // merely to claim undo coverage for a control the user cannot edit.
        let expected: &NSResponder = &session.views.shortcuts;
        if !panel
            .firstResponder()
            .is_some_and(|actual| std::ptr::eq(&*actual, expected))
        {
            return Err("设置窗口没有保持原快捷键选择器焦点".into());
        }
    } else {
        // Observe the field editor AppKit actually installed for the original focused text field.
        // The production observation verifies its session identity and exact native undo manager.
        let editor = session
            .views
            .input
            .currentEditor()
            .and_then(|editor| editor.downcast::<NSTextView>().ok())
            .ok_or("内建输入框没有实际原生文本编辑器")?;
        let expected: &NSResponder = &editor;
        if !panel
            .firstResponder()
            .is_some_and(|actual| std::ptr::eq(&*actual, expected))
        {
            return Err("内建输入框实际编辑器没有键盘焦点".into());
        }
        let owners = session.controller.ivars();
        owners
            .borrowed
            .observe_editing(&session.views.input, &owners.undo, &owners.owned)?;
        // Repeated real responder queries must return the same cached history. This exercises the
        // concrete native lending path, without an injected retain callback or fabricated owner.
        let first = editor.undoManager().ok_or("原生编辑器未返回会话撤销历史")?;
        let second = editor.undoManager().ok_or("重复查询丢失原会话撤销历史")?;
        if !std::ptr::eq(&*first, &*second) {
            return Err("内建编辑器重复查询改变了撤销历史身份".into());
        }
    }
    let owners = session.controller.ivars();
    owners.undo.prepare_close()?;
    owners.field_editor.prepare_close()?;
    // No strong graph owner or lazy history loan escapes this function. Actual native release
    // remains proved by the existing production cohort, strict cache checks, and Host barrier.
    let window: &NSWindow = panel;
    Ok(Weak::from(window))
}

/// Check actual visible content children at each native size. Scroll-view internals retain their
/// own clipping/layout behavior; the surrounding top-level controls must never overlap or escape.
fn verify_top_level_geometry(content: &NSView, expected_title: &str) -> Result<(), String> {
    let bounds = content.bounds();
    let children = content.subviews();
    if children.count() > 64 {
        // A bounded capability view cannot silently turn an unexpected native hierarchy into
        // an unbounded observation or claim that only the first few controls were checked.
        return Err("内建顶层视图超过有限观察范围".into());
    }
    let title = NSString::from_str(expected_title);
    let mut heading = false;
    let mut visible: Vec<NSRect> = Vec::new();
    for index in 0..children.count() {
        let child = children.objectAtIndex(index);
        if child.isHiddenOrHasHiddenAncestor() {
            // Inactive capability controls are real owners but do not consume visible layout space.
            continue;
        }
        let frame = child.frame();
        if !inside(frame, bounds) {
            // Geometry contains no text or user data; retain the exact finite failure facts so
            // the owning production layout can be corrected without relaxing this observation.
            return Err(format!(
                "内建实际控件在缩放后越过内容区域 index={index} frame={frame:?} bounds={bounds:?}"
            ));
        }
        if let Some(other) = visible.iter().find(|other| overlaps(frame, **other)) {
            // Compare real view frames rather than copied construction constants. Decorative labels,
            // interactive controls, and the clipboard scroll container share this visible contract.
            return Err(format!(
                "内建实际顶层控件在缩放后发生重叠 index={index} frame={frame:?} other={other:?} bounds={bounds:?}"
            ));
        }
        if let Some(label) = child.downcast_ref::<NSTextField>()
            && label.stringValue().isEqualToString(&title)
        {
            // A native window title alone is insufficient for an undecorated window: the localized
            // capability heading must remain visible within the actual top navigation region.
            heading = frame.origin.y + frame.size.height >= bounds.size.height - 64.0;
        }
        visible.push(frame);
    }
    if !heading {
        return Err("无边框内建窗口缺少实际可见的中文顶部标题".into());
    }
    Ok(())
}

/// Check the original conversion row specifically, using its two real controls and visible arrow.
/// This stays inside the caller's resize transaction, so every failure restores the original frame.
fn verify_unit_choices(from: &NSView, to: &NSView, content: &NSView) -> Result<(), String> {
    // SAFETY: The active session retains both controls and the window's content graph throughout
    // this synchronous main-thread observation. Their parent views cannot disappear during lookup.
    let (from_parent, to_parent) = unsafe { (from.superview(), to.superview()) };
    let from_parent = from_parent.ok_or("单位换算源选择器没有实际行容器")?;
    let to_parent = to_parent.ok_or("单位换算目标选择器没有实际行容器")?;
    if !std::ptr::eq(&*from_parent, &*to_parent) {
        // Coordinates are comparable only when both production selectors belong to the same row.
        return Err("单位换算选择器未共享同一实际行容器".into());
    }
    // SAFETY: The same retained content graph protects this container's nonowning parent link.
    if !unsafe { from_parent.superview() }.is_some_and(|parent| std::ptr::eq(&*parent, content)) {
        return Err("单位换算行容器不属于原窗口内容区域".into());
    }
    if from.isHiddenOrHasHiddenAncestor() || to.isHiddenOrHasHiddenAncestor() {
        // Hidden selectors cannot pass a visible-layout observation using otherwise plausible frames.
        return Err("单位换算实际源或目标选择器不可见".into());
    }
    let children = from_parent.subviews();
    let arrow_text = NSString::from_str("→");
    let mut arrow = None;
    for index in 0..children.count() {
        let child = children.objectAtIndex(index);
        if child.isHiddenOrHasHiddenAncestor() {
            continue;
        }
        if let Some(label) = child.downcast_ref::<NSTextField>()
            && label.stringValue().isEqualToString(&arrow_text)
        {
            // One visible direction glyph belongs between these exact selectors; duplicated arrows
            // are an actual UI regression rather than another accepted decorative representation.
            if arrow.replace(child.frame()).is_some() {
                return Err("单位换算行出现重复方向箭头".into());
            }
        }
    }
    let arrow = arrow.ok_or("单位换算行缺少实际可见的方向箭头")?;
    let bounds = from_parent.bounds();
    let source = from.frame();
    let destination = to.frame();
    if !inside(source, bounds) || !inside(destination, bounds) || !inside(arrow, bounds) {
        return Err(format!(
            "单位换算行内控件越界 source={source:?} destination={destination:?} arrow={arrow:?} bounds={bounds:?}"
        ));
    }
    if overlaps(source, destination) || overlaps(source, arrow) || overlaps(destination, arrow) {
        // Compare all three real frames at every requested size. A container fitting the content
        // does not excuse an autoresizing-mask regression that overlaps its internal controls.
        return Err(format!(
            "单位换算行内选择器或方向箭头重叠 source={source:?} destination={destination:?} arrow={arrow:?} bounds={bounds:?}"
        ));
    }
    Ok(())
}

/// Reject nonfinite or negative geometry before using it as native containment evidence.
fn inside(frame: NSRect, bounds: NSRect) -> bool {
    const TOLERANCE: f64 = 0.25;
    [
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    ]
    .iter()
    .all(|value| value.is_finite())
        && frame.size.width >= 0.0
        && frame.size.height >= 0.0
        && frame.origin.x >= bounds.origin.x - TOLERANCE
        && frame.origin.y >= bounds.origin.y - TOLERANCE
        && frame.origin.x + frame.size.width <= bounds.origin.x + bounds.size.width + TOLERANCE
        && frame.origin.y + frame.size.height <= bounds.origin.y + bounds.size.height + TOLERANCE
}

/// Shared edges are permitted; a positive intersection beyond subpixel rounding is visible overlap.
fn overlaps(left: NSRect, right: NSRect) -> bool {
    const TOLERANCE: f64 = 0.25;
    left.origin.x.max(right.origin.x) + TOLERANCE
        < (left.origin.x + left.size.width).min(right.origin.x + right.size.width)
        && left.origin.y.max(right.origin.y) + TOLERANCE
            < (left.origin.y + left.size.height).min(right.origin.y + right.size.height)
}
