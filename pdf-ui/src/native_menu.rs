//! Menu natif macOS (Sprint 11-12, sprint.md) — construit une vraie
//! `NSMenu` (barre de menus système), pas une barre de menus dessinée par
//! `egui`. Décision de dépendance actée dans sprint.md : `objc2` +
//! `objc2-app-kit`/`objc2-foundation` plutôt que `cacao`.
//!
//! Deux catégories d'items :
//! - Actions **standard AppKit** (Quitter, Fermer, Réduire, Zoomer, Plein
//!   écran) : cible `None` (`nil`), le sélecteur est envoyé le long de la
//!   chaîne de répondeurs — `NSWindow`/`NSApplication` l'implémentent déjà,
//!   aucun code Rust supplémentaire n'est nécessaire pour que ça fonctionne.
//! - Actions **propres à l'application** (Ouvrir, Exporter une copie,
//!   basculer l'apparence) : nécessitent une cible réelle, donc une classe
//!   Objective-C définie ici (`MenuTarget`, via `objc2::define_class!`) dont
//!   les méthodes poussent une [`MenuCommand`] dans un canal MPSC lu à
//!   chaque frame par `ViewerApp::update` (voir `main.rs`).
//!
//! `⌘Z`/`⌘⇧Z` (Undo/Redo) et `⌘S` (Enregistrer) sont câblés depuis que
//! `pdf-edit::EditSession` existe réellement (Sprints 13-17) — voir
//! `MenuCommand::Undo`/`Redo`/`Save`. `⌘S` fait maintenant un vrai
//! "Enregistrer" (écrit dans le fichier ouvert) ; "Exporter une copie…" est
//! resté, déplacé sur `⇧⌘S`.
//!
//! `⌘P` (Imprimer…, Sprint 21, #48) délègue à Aperçu via AppleScript
//! (`ViewerApp::print_document`, `main.rs`) plutôt qu'un pipeline
//! `NSPrintOperation` maison — cette app n'a pas de vraie `NSView` de
//! contenu à imprimer (tout passe par une texture `egui`/`wgpu`), et cette
//! approche donne gratuitement l'aperçu et la sélection de pages du système.
//!
//! `⌘T` (Nouvel onglet) et `⌘W` (Fermer l'onglet, Sprint 49) : onglets
//! multi-documents dans une seule fenêtre (`ViewerApp` porte `Vec<DocumentTab>`,
//! voir `main.rs`) — `⌘W` ferme l'onglet actif, pas la fenêtre entière,
//! contrairement à la sémantique standard AppKit de `performClose:`
//! (toujours disponible, mais déplacée sur `⇧⌘W`).
//!
//! **Non fait** (voir sprint.md) : Quick Look — nécessiterait une extension
//! d'application séparée (`.qlgenerator`/`appex`, bundle et cible de build
//! distincts d'Xcode), hors périmètre d'un simple binaire `cargo`.

use std::cell::RefCell;
use std::sync::mpsc::{channel, Receiver, Sender};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    NSEventModifierFlags, NSMenu, NSMenuItem,
};
use objc2_foundation::{ns_string, NSObject, NSObjectProtocol, NSString};

/// Commande émise par un item de menu propre à l'application, à traiter au
/// prochain `update()` de `ViewerApp` (jamais directement depuis le
/// sélecteur Objective-C : on reste sur le thread principal mais on évite
/// de coupler l'état `egui` au callback AppKit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    OpenDocument,
    ExportCopyAs,
    ToggleDarkMode,
    Save,
    Undo,
    Redo,
    Print,
    /// "Fermer l'onglet" (Sprint 49, `⌘W`) — remplace `performClose:` comme
    /// action par défaut de `⌘W` puisque l'app gère désormais plusieurs
    /// documents en onglets dans une seule fenêtre : fermer *la fenêtre*
    /// n'a plus le même sens que fermer *l'onglet actif*. `performClose:`
    /// reste disponible sur `⇧⌘W` pour fermer réellement la fenêtre.
    CloseTab,
}

struct MenuTargetIvars {
    sender: Sender<MenuCommand>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(openDocument:))]
        fn open_document(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::OpenDocument);
        }

        #[unsafe(method(exportCopyAs:))]
        fn export_copy_as(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::ExportCopyAs);
        }

        #[unsafe(method(toggleDarkMode:))]
        fn toggle_dark_mode(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::ToggleDarkMode);
        }

        #[unsafe(method(saveDocument:))]
        fn save_document(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::Save);
        }

        #[unsafe(method(undoEdit:))]
        fn undo_edit(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::Undo);
        }

        #[unsafe(method(redoEdit:))]
        fn redo_edit(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::Redo);
        }

        #[unsafe(method(printDocument:))]
        fn print_document(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::Print);
        }

        #[unsafe(method(closeTab:))]
        fn close_tab(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().sender.send(MenuCommand::CloseTab);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, sender: Sender<MenuCommand>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { sender });
        unsafe { msg_send![super(this), init] }
    }
}

/// Poignée conservée par `ViewerApp` : garde `MenuTarget` en vie (sinon
/// AppKit se retrouverait avec une cible de menu désallouée) et expose le
/// bout récepteur du canal de commandes.
pub struct NativeMenu {
    // Jamais lu directement : sa seule raison d'être est de garder `target`
    // vivant tant que `NativeMenu` existe (les `NSMenuItem` ne font que
    // référencer `target` sans en être propriétaires).
    _target: Retained<MenuTarget>,
    commands: Receiver<MenuCommand>,
}

impl NativeMenu {
    /// Construit et installe la barre de menus native, en remplacement de
    /// celle (minimale) que `winit` installe par défaut sur macOS.
    pub fn install(mtm: MainThreadMarker) -> Self {
        let (sender, commands) = channel();
        let target = MenuTarget::new(mtm, sender);
        let target_obj: &AnyObject = &target;

        let app = NSApplication::sharedApplication(mtm);
        let main_menu = NSMenu::new(mtm);

        main_menu.addItem(&app_menu_item(mtm));
        main_menu.addItem(&file_menu_item(mtm, target_obj));
        main_menu.addItem(&edit_menu_item(mtm, target_obj));
        main_menu.addItem(&view_menu_item(mtm, target_obj));
        main_menu.addItem(&window_menu_item(mtm));

        app.setMainMenu(Some(&main_menu));

        // Requis pour un binaire lancé "nu" (`cargo run`, pas un vrai `.app`
        // bundle avec un `Info.plist`) : sans ça, l'app peut rester en
        // arrière-plan côté AppKit même si sa fenêtre est visible, et sa
        // barre de menus ne devient jamais celle affichée par le système.
        // Voir l'exemple officiel `objc2`/`objc2-app-kit`
        // (`hello_world_app.rs`), qui documente exactement ce cas.
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);

        Self {
            _target: target,
            commands,
        }
    }

    /// À appeler une fois par frame : retourne toutes les commandes émises
    /// depuis le dernier appel (généralement 0 ou 1, un utilisateur ne
    /// spamme pas les items de menu à la fréquence d'affichage).
    pub fn drain_commands(&self) -> Vec<MenuCommand> {
        self.commands.try_iter().collect()
    }
}

fn item_with_action(
    mtm: MainThreadMarker,
    title: &str,
    action: objc2::runtime::Sel,
    key_equivalent: &str,
    target: Option<&AnyObject>,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key_equivalent),
        )
    };
    unsafe { item.setTarget(target) };
    item
}

fn app_menu_item(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let submenu = NSMenu::new(mtm);
    // Cible `None` : "terminate:" est envoyé le long de la chaîne de
    // répondeurs, `NSApplication` (en bout de chaîne) l'implémente déjà.
    submenu.addItem(&item_with_action(
        mtm,
        "Quitter PapyrusPDF",
        sel!(terminate:),
        "q",
        None,
    ));
    let item = NSMenuItem::new(mtm);
    item.setSubmenu(Some(&submenu));
    item
}

fn file_menu_item(mtm: MainThreadMarker, target: &AnyObject) -> Retained<NSMenuItem> {
    let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Fichier"));
    submenu.addItem(&item_with_action(
        mtm,
        "Ouvrir…",
        sel!(openDocument:),
        "o",
        Some(target),
    ));
    // "Nouvel onglet" (Sprint 49) : même action qu'"Ouvrir…" — les deux
    // ouvrent un fichier dans un nouvel onglet, seul le raccourci diffère
    // (convention macOS `⌘T` pour un nouvel onglet).
    submenu.addItem(&item_with_action(
        mtm,
        "Nouvel onglet",
        sel!(openDocument:),
        "t",
        Some(target),
    ));
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    submenu.addItem(&item_with_action(
        mtm,
        "Enregistrer",
        sel!(saveDocument:),
        "s",
        Some(target),
    ));
    let export_copy = item_with_action(
        mtm,
        "Exporter une copie…",
        sel!(exportCopyAs:),
        "s",
        Some(target),
    );
    export_copy
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    submenu.addItem(&export_copy);
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    submenu.addItem(&item_with_action(
        mtm,
        "Imprimer…",
        sel!(printDocument:),
        "p",
        Some(target),
    ));
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    submenu.addItem(&item_with_action(
        mtm,
        "Fermer l'onglet",
        sel!(closeTab:),
        "w",
        Some(target),
    ));
    // `performClose:` (ferme réellement la fenêtre) : implémenté par
    // `NSWindow` lui-même, cible `None` — déplacé sur `⇧⌘W` puisque `⌘W`
    // ferme maintenant l'onglet actif (voir `MenuCommand::CloseTab`).
    let close_window = item_with_action(mtm, "Fermer la fenêtre", sel!(performClose:), "w", None);
    close_window
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    submenu.addItem(&close_window);
    let item = NSMenuItem::new(mtm);
    item.setTitle(ns_string!("Fichier"));
    item.setSubmenu(Some(&submenu));
    item
}

fn edit_menu_item(mtm: MainThreadMarker, target: &AnyObject) -> Retained<NSMenuItem> {
    let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Édition"));
    submenu.addItem(&item_with_action(
        mtm,
        "Annuler",
        sel!(undoEdit:),
        "z",
        Some(target),
    ));
    let redo = item_with_action(mtm, "Rétablir", sel!(redoEdit:), "z", Some(target));
    redo.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    submenu.addItem(&redo);
    let item = NSMenuItem::new(mtm);
    item.setTitle(ns_string!("Édition"));
    item.setSubmenu(Some(&submenu));
    item
}

fn view_menu_item(mtm: MainThreadMarker, target: &AnyObject) -> Retained<NSMenuItem> {
    let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Affichage"));
    submenu.addItem(&item_with_action(
        mtm,
        "Basculer le mode sombre",
        sel!(toggleDarkMode:),
        "",
        Some(target),
    ));
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    // `toggleFullScreen:` : implémenté par `NSWindow`, cible `None`.
    // Raccourci standard macOS : Ctrl+Cmd+F (Shift n'est pas nécessaire ici,
    // donc pas besoin d'exposer les modificateurs sur les autres items).
    let fullscreen = item_with_action(
        mtm,
        "Entrer en plein écran",
        sel!(toggleFullScreen:),
        "f",
        None,
    );
    fullscreen.setKeyEquivalentModifierMask(
        NSEventModifierFlags::Control | NSEventModifierFlags::Command,
    );
    submenu.addItem(&fullscreen);
    let item = NSMenuItem::new(mtm);
    item.setTitle(ns_string!("Affichage"));
    item.setSubmenu(Some(&submenu));
    item
}

fn window_menu_item(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Fenêtre"));
    // `performMiniaturize:`/`performZoom:` : implémentés par `NSWindow`,
    // cible `None`.
    submenu.addItem(&item_with_action(
        mtm,
        "Réduire",
        sel!(performMiniaturize:),
        "m",
        None,
    ));
    submenu.addItem(&item_with_action(
        mtm,
        "Zoomer",
        sel!(performZoom:),
        "",
        None,
    ));
    let item = NSMenuItem::new(mtm);
    item.setTitle(ns_string!("Fenêtre"));
    item.setSubmenu(Some(&submenu));
    item
}

thread_local! {
    static DARK_MODE: RefCell<bool> = const { RefCell::new(false) };
}

/// Force l'apparence de l'app (clair/sombre) via `NSApplication.appearance`
/// plutôt que de suivre l'apparence système — un vrai bouton "mode sombre",
/// pas juste une lecture passive du thème système. Retourne le nouvel état
/// pour que l'appelant synchronise les couleurs `egui` (`ctx.set_visuals`).
pub fn toggle_dark_mode(mtm: MainThreadMarker) -> bool {
    let now_dark = DARK_MODE.with(|cell| {
        let mut dark = cell.borrow_mut();
        *dark = !*dark;
        *dark
    });
    let app = NSApplication::sharedApplication(mtm);
    let name = if now_dark {
        unsafe { NSAppearanceNameDarkAqua }
    } else {
        unsafe { NSAppearanceNameAqua }
    };
    let appearance = NSAppearance::appearanceNamed(name);
    app.setAppearance(appearance.as_deref());
    now_dark
}
