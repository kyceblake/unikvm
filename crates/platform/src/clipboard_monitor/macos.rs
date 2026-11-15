#![allow(unsafe_code)]

use std::{
    cell::{Cell, RefCell},
    fs::{OpenOptions, TryLockError},
    io,
    os::unix::fs::OpenOptionsExt,
};

use objc2::rc::{Retained, autoreleasepool};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertSecondButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSBackingStoreType, NSButton, NSColor, NSFloatingWindowLevel, NSFont, NSMenu, NSMenuItem,
    NSPanel, NSProgressIndicator, NSProgressIndicatorStyle, NSScreen, NSStatusBar,
    NSStatusBarButton, NSStatusItem, NSTextField, NSVariableStatusItemLength, NSView,
    NSWindowStyleMask,
};
use objc2_app_kit::{NSEventMask, NSImage, NSImageNameNetwork};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer};
use unikvm_protocol::tray::{Command, Connection, Phase, Transfer, View};

use super::{
    Snapshot, TITLE, TransferDisplay, UI_POLL_MS, clipboard_status, connection, peer_label, size,
    tooltip,
};

const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 94.0;

pub(super) fn show(
    read: impl Fn() -> Snapshot + 'static,
    send: impl Fn(Command) + 'static,
) -> io::Result<()> {
    let main = main_thread()?;
    let directory = crate::local::Endpoint::for_current_user()?.clipboard_directory()?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(directory.join("tray.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
            return Err(io::Error::other("could not start the menu bar helper"));
        }
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let button = item
            .button(main)
            .ok_or_else(|| io::Error::other("create the menu bar button"))?;
        // SAFETY: AppKit supplies this system image name.
        if let Some(image) = NSImage::imageNamed(unsafe { NSImageNameNetwork }) {
            image.setTemplate(true);
            button.setImage(Some(&image));
        } else {
            button.setTitle(&NSString::from_str(TITLE));
        }
        let target = Delegate::alloc(main).set_ivars(Tray {
            read: Box::new(read),
            send: Box::new(send),
            item,
            button,
            last_menu: RefCell::new(Vec::new()),
            last_tip: RefCell::new(String::new()),
            menu: RefCell::new(None),
            progress: RefCell::new([None, None]),
        });
        // SAFETY: NSObject initializes the allocated subclass with installed ivars.
        let target: Retained<Delegate> = unsafe { msg_send![super(target), init] };
        // SAFETY: openMenu: accepts the native button and the tray retains its target.
        unsafe {
            target.ivars().button.setTarget(Some(&*target));
            target.ivars().button.setAction(Some(sel!(openMenu:)));
        }
        target
            .ivars()
            .button
            .sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        target.refresh_view();
        // SAFETY: The retained target implements refresh: with one NSTimer.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                UI_POLL_MS as f64 / 1000.0,
                &target,
                sel!(refresh:),
                None,
                true,
            )
        };
        app.finishLaunching();
        app.run();
        timer.invalidate();
        bar.removeStatusItem(&target.ivars().item);
        Ok(())
    })
}

struct Tray {
    read: Box<dyn Fn() -> Snapshot>,
    send: Box<dyn Fn(Command)>,
    item: Retained<NSStatusItem>,
    button: Retained<NSStatusBarButton>,
    last_menu: RefCell<Vec<String>>,
    last_tip: RefCell<String>,
    menu: RefCell<Option<Retained<NSMenu>>>,
    progress: RefCell<[Option<Panel>; 2]>,
}

const CONNECT: isize = 10;
const DISCONNECT: isize = 11;
const EXIT: isize = 12;

define_class!(
    // SAFETY: AppKit invokes these selectors only on the owning main thread.
    #[unsafe(super(NSObject))]
    #[name = "UniKVMTrayDelegate"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Tray]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    impl Delegate {
        #[unsafe(method(refresh:))]
        fn refresh(&self, _timer: &NSTimer) {
            self.refresh_view();
        }

        #[unsafe(method(openMenu:))]
        fn open_menu(&self, button: &NSStatusBarButton) {
            let menu = self.ivars().menu.borrow().clone();
            if let Some(menu) = menu {
                menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::ZERO, Some(button));
            }
        }

        #[unsafe(method(menuAction:))]
        fn menu_action(&self, item: &NSMenuItem) {
            let command = match item.tag() {
                CONNECT => Command::Connect,
                DISCONNECT => Command::Disconnect,
                EXIT => Command::Exit,
                _ => return,
            };
            (self.ivars().send)(command);
        }

        #[unsafe(method(cancelTransfer:))]
        fn cancel_transfer(&self, button: &NSButton) {
            if let Some(Some(panel)) = self.ivars().progress.borrow_mut().get_mut(button.tag() as usize) {
                panel.display.cancel(&*self.ivars().send);
                button.setEnabled(false);
            }
        }
    }
);

impl Delegate {
    fn refresh_view(&self) {
        let tray = self.ivars();
        let snapshot = (tray.read)();
        if snapshot.close {
            NSApplication::sharedApplication(self.mtm()).stop(None);
            return;
        }
        let view = snapshot.view.as_ref();
        let tip = tooltip(view, usize::MAX);
        if *tray.last_tip.borrow() != tip {
            tray.button.setToolTip(Some(&NSString::from_str(&tip)));
            tray.last_tip.replace(tip);
        }
        self.update_menu(view);
        for (direction, panel) in tray.progress.borrow_mut().iter_mut().enumerate() {
            let transfer = view.and_then(|view| view.transfers[direction].as_ref());
            if transfer.is_some() && panel.is_none() {
                *panel = Some(Panel::new(self, direction));
            }
            if let Some(panel) = panel {
                panel.update(transfer, direction);
            }
        }
    }

    fn update_menu(&self, view: Option<&View>) {
        let mut labels = vec![connection(view).to_owned()];
        let mut action = None;
        if let Some(view) = view {
            labels.extend(view.peers.iter().map(peer_label));
            labels.extend((0..2).filter_map(|direction| clipboard_status(view, direction)));
            action = match view.connection {
                Connection::Disconnected => Some(("Connect", CONNECT)),
                Connection::Online => Some(("Disconnect", DISCONNECT)),
                _ => None,
            };
        }
        let mut signature = labels.clone();
        signature.extend(action.map(|(title, _)| title.to_owned()));
        if *self.ivars().last_menu.borrow() == signature {
            return;
        }
        self.ivars().last_menu.replace(signature);
        let menu = NSMenu::new(self.mtm());
        menu.setAutoenablesItems(false);
        for label in labels {
            self.add_menu_item(&menu, &label, None);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        if let Some((label, tag)) = action {
            self.add_menu_item(&menu, label, Some(tag));
        }
        self.add_menu_item(&menu, "Exit completely", Some(EXIT));
        self.ivars().menu.replace(Some(menu));
    }

    fn add_menu_item(&self, menu: &NSMenu, title: &str, tag: Option<isize>) {
        // SAFETY: menuAction: takes a menu item and the tray retains its target.
        let item = unsafe {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(title),
                tag.map(|_| sel!(menuAction:)),
                &NSString::from_str(""),
            );
            if tag.is_some() {
                item.setTarget(Some(self));
            }
            item
        };
        item.setTag(tag.unwrap_or_default());
        item.setEnabled(tag.is_some());
        menu.addItem(&item);
    }
}

struct Panel {
    window: Retained<NSPanel>,
    heading: Retained<NSTextField>,
    details: Retained<NSTextField>,
    bar: Retained<NSProgressIndicator>,
    cancel: Retained<NSButton>,
    display: TransferDisplay,
    last_id: Cell<Option<u64>>,
}

impl Panel {
    fn new(target: &Delegate, direction: usize) -> Self {
        let main = target.mtm();
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT));
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(main),
            bounds,
            NSWindowStyleMask::Titled | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setTitle(&NSString::from_str(TITLE));
        window.setLevel(NSFloatingWindowLevel);
        window.setHidesOnDeactivate(false);
        window.setBecomesKeyOnlyIfNeeded(true);
        let view = NSView::initWithFrame(NSView::alloc(main), bounds);
        let heading = NSTextField::labelWithString(&NSString::from_str(""), main);
        heading.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
        heading.setFrame(rect(15.0, 65.0, 330.0, 20.0));
        view.addSubview(&heading);
        let details = NSTextField::labelWithString(&NSString::from_str(""), main);
        details.setTextColor(Some(&NSColor::secondaryLabelColor()));
        details.setFrame(rect(15.0, 41.0, 330.0, 19.0));
        view.addSubview(&details);
        let bar = NSProgressIndicator::initWithFrame(
            NSProgressIndicator::alloc(main),
            rect(15.0, 17.0, 246.0, 12.0),
        );
        bar.setStyle(NSProgressIndicatorStyle::Bar);
        bar.setIndeterminate(false);
        view.addSubview(&bar);
        // SAFETY: cancelTransfer: takes a button and the tray owns the target.
        let cancel = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Cancel"),
                Some(target),
                Some(sel!(cancelTransfer:)),
                main,
            )
        };
        cancel.setTag(direction as isize);
        cancel.setFrame(rect(271.0, 10.0, 76.0, 24.0));
        view.addSubview(&cancel);
        window.setContentView(Some(&view));
        Self {
            window,
            heading,
            details,
            bar,
            cancel,
            display: TransferDisplay::new(),
            last_id: Cell::new(None),
        }
    }

    fn update(&mut self, transfer: Option<&Transfer>, direction: usize) {
        let Some((item, (visible, changed))) =
            transfer.and_then(|item| self.display.observe(Some(item)).map(|show| (item, show)))
        else {
            self.window.orderOut(None);
            return;
        };
        if !visible {
            self.window.orderOut(None);
            return;
        }
        let new_id = self.last_id.replace(Some(item.id)) != Some(item.id);
        if changed {
            let heading = match (direction, item.phase) {
                (0, Phase::Copying) => format!("Sending to {}", item.peer),
                (_, Phase::Copying) => format!("Receiving from {}", item.peer),
                (0, Phase::Ready) => format!("Ready to paste on {}", item.peer),
                (_, Phase::Ready) => "Ready to paste here".to_owned(),
                (_, Phase::Failed) => "Transfer failed".to_owned(),
                (_, Phase::Cancelled) => "Transfer cancelled".to_owned(),
            };
            self.heading.setStringValue(&NSString::from_str(&heading));
            self.cancel.setHidden(item.phase != Phase::Copying);
            if new_id {
                self.cancel.setEnabled(true);
            }
        }
        self.details.setStringValue(&NSString::from_str(&format!(
            "{} · {} / {}",
            item.label,
            size(item.bytes),
            size(item.total)
        )));
        self.bar.setMaxValue(item.total.max(1) as f64);
        self.bar.setDoubleValue(if item.phase == Phase::Ready {
            item.total.max(1)
        } else {
            item.bytes
        } as f64);
        if !self.window.isVisible() {
            if let Some(screen) = NSScreen::mainScreen(self.window.mtm()) {
                let space = screen.visibleFrame();
                let frame = self.window.frame();
                self.window.setFrameOrigin(NSPoint::new(
                    space.origin.x + space.size.width - WIDTH - 18.0,
                    space.origin.y + space.size.height
                        - (frame.size.height + 12.0) * (direction + 1) as f64,
                ));
            }
            self.window.orderFrontRegardless();
        }
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn main_thread() -> io::Result<MainThreadMarker> {
    MainThreadMarker::new().ok_or_else(|| io::Error::other("AppKit requires the main thread"))
}

pub(super) fn approve(item: &str, size: &str) -> io::Result<bool> {
    let main = main_thread()?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        if !app.setActivationPolicy(NSApplicationActivationPolicy::Regular) {
            return Err(io::Error::other("could not activate the clipboard prompt"));
        }
        app.finishLaunching();
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let alert = NSAlert::new(main);
        alert.setMessageText(&NSString::from_str("Receive a clipboard copy?"));
        alert.setInformativeText(&NSString::from_str(&format!(
            "Your paired computer offered {item} ({size})."
        )));
        alert.addButtonWithTitle(&NSString::from_str("Decline"));
        alert.addButtonWithTitle(&NSString::from_str("Receive"));
        Ok(alert.runModal() == NSAlertSecondButtonReturn)
    })
}
