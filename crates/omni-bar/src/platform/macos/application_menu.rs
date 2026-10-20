//! 进程菜单独立调度显式退出；编辑动作仍交给当前原生响应者。
//!
//! 菜单不持有插件或文档；明确的退出目标避免异步退出时临时保活前台响应链。

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

define_class!(
    /// 菜单拥有不含业务载荷的目标，退出等待期间不借用前台响应者或控制器。
    // SAFETY: 子类不新增实例字段，保持单参数 action 的 ABI 和主线程约束。
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct QuitMenuTarget;

    // SAFETY: 沿用已初始化的 NSObject 身份，不覆盖内存管理或线程行为。
    unsafe impl NSObjectProtocol for QuitMenuTarget {}

    impl QuitMenuTarget {
        #[unsafe(method(requestOmniTermination:))]
        /// 只预约一次退出；菜单 action 返回后才进入原生 terminate:/delegate 协议。
        fn request_termination(&self, sender: Option<&AnyObject>) {
            #[cfg(feature = "native-acceptance")]
            {
                // 验收只认原菜单项的真实action，不合成用户明确拒绝的退出快捷键。
                super::acceptance_termination::record_menu_action(sender);
            }
            #[cfg(not(feature = "native-acceptance"))]
            let _ = sender;
            if let Err(error) = super::termination::request_application_termination() {
                // 调度失败保留当前前台，避免跳过保存事务直接终止进程。
                eprintln!("menu termination could not be scheduled: {error}");
            }
            #[cfg(feature = "native-acceptance")]
            {
                // 父进程要求此标量先于 delegate 回复，以发现菜单 action 栈内重新引入的等待。
                super::acceptance_termination::record_menu_request_returned();
            }
        }
    }
);

impl QuitMenuTarget {
    /// 主线程创建无业务载荷的 action 目标，由菜单作用域持有至原菜单恢复。
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(());
        // SAFETY: 单元实例字段已初始化；NSObject.init 消费 +1 分配并返回同一主线程类型。
        unsafe { msg_send![super(allocated), init] }
    }
}

/// 初始化失败或事件循环显式停止时，恢复此前的应用菜单。
pub(super) struct ApplicationMenuScope<'a> {
    application: &'a NSApplication,
    previous: Option<Retained<NSMenu>>,
    _quit_target: Retained<QuitMenuTarget>,
}

impl<'a> ApplicationMenuScope<'a> {
    /// 在产品事件循环接收按键前安装所需原生菜单。
    pub(super) fn install(application: &'a NSApplication, mtm: MainThreadMarker) -> Self {
        // 只保留原进程菜单，恢复过程不需要持有任何前台业务对象。
        let previous = application.mainMenu();
        // 退出目标只归进程菜单所有，与窗口 delegate 的资源生命周期分开。
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
            // 退出仍由显式菜单动作进入原事务；不注册Cmd+Q，也不选择任何替代按键。
            "",
            false,
            mtm,
        );
        // 原菜单退出与插件返回各有实际意图；没有按键不影响保存、释放和异步退出顺序。
        // 显式目标避免 nil target 沿输入响应链查找时临时保活插件窗口。
        // SAFETY: 作用域持有该主线程目标，Drop 恢复原菜单后才释放它。
        unsafe { quit.setTarget(Some(&*quit_target)) };

        // 中文菜单沿用已有编辑 selector 和按键，由当前原生输入框或文档历史处理。
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
            // 固定菜单只注册响应者动作，文档状态和撤销历史仍归实际编辑器所有。
            add_action(&edit, title, selector, key, shift, mtm);
        }
        // NSApplication 接管菜单树；发布后即可归还本次构造的局部强引用。
        application.setMainMenu(Some(&main));
        Self {
            application,
            previous,
            _quit_target: quit_target,
        }
    }
}

/// 默认创建响应链菜单项；进程级动作由调用方设置明确的目标对象。
fn add_action(
    menu: &NSMenu,
    title: &str,
    selector: Sel,
    key: &str,
    shift: bool,
    mtm: MainThreadMarker,
) -> Retained<NSMenuItem> {
    // SAFETY: 调用方仅传入已声明且符合 NSMenuItem 单对象 action ABI 的 selector。
    // 初始 nil target 交给响应链；退出项随后绑定菜单作用域拥有的目标。
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(selector),
            &NSString::from_str(key),
        )
    };
    let modifiers = if shift {
        // 重做保留原 Command-Shift-Z，显式声明 Shift，避免依赖大写字母别名。
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift
    } else {
        // 其他已有编辑动作使用其明确的 Command 组合；空键退出项不会注册按键。
        NSEventModifierFlags::Command
    };
    item.setKeyEquivalentModifierMask(modifiers);
    menu.addItem(&item);
    // 菜单已经拥有该项；返回的局部引用只用于设置目标，不持有前台对象图。
    item
}

impl Drop for ApplicationMenuScope<'_> {
    fn drop(&mut self) {
        // 停止或失败的初始化先恢复原生菜单状态，随后再释放本作用域保存的原菜单。
        self.application.setMainMenu(self.previous.as_deref());
    }
}
