//! Viewer PDF (egui) — architecture.md §8.1 : "commencer le prototype en
//! egui pour valider les flux, migrer le chrome vers natif ensuite". Ce
//! binaire parle à `pdf-app::Session` pour l'état de session (document
//! ouvert, page courante, rendu) — voir STATUS.md, ce n'est plus un
//! raccourci direct vers `pdf-core`/`pdf-render`.
//!
//! Fonctionnalités : ouverture de fichier (dialogue natif via `rfd`),
//! navigation page suivante/précédente, zoom par boutons, par
//! molette+Ctrl/pincement trackpad (`egui::InputState::zoom_delta`,
//! re-rasterisation à chaque cran, pas un agrandissement d'image) **et**
//! "Ajuster à la largeur"/"Ajuster à la page", recherche texte
//! (`Session::find_pages_containing`) qui saute d'occurrence en occurrence
//! page par page **avec surlignage** des résultats sur la page affichée
//! (`Session::find_matches_on_current_page`), panneau de miniatures
//! cliquables et panneau de signets (`/Outlines`, `Session::outline`) pour
//! naviguer directement à une page, un mode **défilement continu**
//! (`egui::ScrollArea::show_rows`, virtualisé : seules les pages proches de
//! la zone visible sont rastérisées) qui affiche toutes les pages empilées
//! verticalement au lieu d'une à la fois, et la **sélection de texte à la
//! souris** (glisser/double-clic/triple-clic sur la page en mode page
//! unique, via `Session::char_index_at_on_current_page`/
//! `selection_on_current_page`) avec copie dans le presse-papiers (bouton
//! ou ⌘C) et surlignage/soulignement/barré `/Highlight`/`/Underline`/
//! `/StrikeOut` (boutons, réutilisent la sélection courante), ajout de
//! texte libre et remplacement de texte par superposition (#30/#40, dialogue
//! modale de saisie), manipulation de pages (insérer/supprimer/pivoter/
//! réordonner par glisser-déposer/fusionner/extraire, panneau miniatures),
//! impression (délégation à Aperçu via AppleScript) et export optimisé.
//!
//! **Onglets multi-documents (Sprint 49)** : `ViewerApp` porte `Vec<DocumentTab>`
//! — chaque onglet a sa propre `Session`, son propre thread de rendu en
//! arrière-plan (`render_worker`) et tout son état d'affichage (zoom,
//! sélection, recherche...), complètement indépendant des autres onglets.
//! `ViewerApp` ne porte que ce qui est réellement global à l'application :
//! la barre de menus native, le backend GPU partagé, et la liste
//! d'onglets elle-même. Voir `DocumentTab` pour l'état par document et
//! `ViewerApp` pour la coordination (barre d'onglets, routage des commandes
//! du menu natif vers l'onglet actif).
//!
//! Raccourcis clavier (`DocumentTab::handle_keyboard_shortcuts`) : ⌘F (focus
//! recherche), ⌘G (focus "aller à la page"), ⌘+/⌘-/⌘0 (zoom), flèches
//! gauche/droite et Page Haut/Bas (page précédente/suivante — désactivés
//! tant qu'un champ de texte a le focus), en plus de ⌘Z/⌘⇧Z
//! (annuler/rétablir), ⌘S (enregistrer), ⌘P (imprimer), ⌘T (nouvel onglet)
//! et ⌘W (fermer l'onglet) câblés via le menu natif (voir `native_menu.rs`).
//!
//! Limitations connues : sélection de texte en mode défilement continu
//! (page unique seulement), indicateur de modifications non sauvegardées,
//! hauteur de ligne du défilement continu dérivée de la page 0 uniquement
//! (documents à pages de tailles hétérogènes mal gérés).

mod native_menu;
mod render_worker;

use eframe::egui;
use native_menu::{MenuCommand, NativeMenu};
use pdf_app::Session;
use render_worker::{RenderKind, RenderWorker};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
/// Identifiant stable du champ de recherche, pour pouvoir lui donner le
/// focus depuis `⌘F` (`handle_keyboard_shortcuts`) sans dépendre de l'ordre
/// d'appel des widgets `egui` dans la frame.
const SEARCH_FIELD_ID: &str = "search_query_field";
/// Identifiant stable du champ "Aller à la page", pour lui donner le focus
/// depuis `⌘G` (`handle_keyboard_shortcuts`), symétrique de `SEARCH_FIELD_ID`.
const GOTO_PAGE_FIELD_ID: &str = "goto_page_field";
/// Échelle de rendu des miniatures (page 612pt de large -> ~92px).
const THUMBNAIL_SCALE: f64 = 0.15;
/// Densité de rendu minimale (pixels bitmap par point PDF), indépendante de
/// `ctx.pixels_per_point()`. Sur un écran externe non-Retina (souvent 1.0),
/// se caler uniquement sur la densité de l'écran donne un rendu peu
/// suréchantillonné (arêtes de glyphes crénelées/floues, visibles surtout
/// sur les fûts verticaux/horizontaux faute de hinting TrueType, voir
/// `pdf-render`). Sur-échantillonner à au moins 2x lisse ces arêtes même
/// quand `pixels_per_point() == 1.0`, au prix d'un peu plus de calcul/mémoire
/// texture.
const MIN_RENDER_DENSITY: f32 = 2.0;

/// Densité de rendu effective à utiliser pour rastériser une page/miniature :
/// au moins `MIN_RENDER_DENSITY`, ou plus si l'écran est un Retina >2x.
fn render_density(ctx: &egui::Context) -> f32 {
    ctx.pixels_per_point().max(MIN_RENDER_DENSITY)
}
/// Jaune translucide pour le surlignage des résultats de recherche.
const HIGHLIGHT_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 85, 10, 90);
/// Bleu translucide pour la sélection de texte à la souris.
const SELECTION_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(20, 60, 110, 90);
/// Contour des annotations existantes (panneau "annotations", Sprint 20).
const ANNOTATION_OUTLINE_COLOR: egui::Color32 = egui::Color32::from_rgb(200, 60, 200);
/// Contour des champs de formulaire texte cliquables (Sprint 23) — distinct
/// de `ANNOTATION_OUTLINE_COLOR` pour ne pas laisser croire qu'un champ est
/// une annotation supprimable comme les autres.
const FORM_FIELD_OUTLINE_COLOR: egui::Color32 = egui::Color32::from_rgb(30, 140, 200);
/// Contour des cases à cocher cliquables (Sprint 52, #43 suite) — même
/// teinte que `FORM_FIELD_OUTLINE_COLOR` (même famille "champ de
/// formulaire"), un remplissage translucide indique en plus l'état coché.
const CHECKBOX_FIELD_OUTLINE_COLOR: egui::Color32 = egui::Color32::from_rgb(30, 140, 200);
/// Taille de police par défaut du texte ajouté/remplacé (Sprint 20) — pas de
/// réglage utilisateur pour l'instant, comme `pdf-cli add-text`/`replace-text`.
const DEFAULT_TEXT_FONT_SIZE: f64 = 14.0;
/// Largeur/hauteur (points page) de la boîte par défaut d'un nouveau texte
/// libre ajouté au clic (Sprint 20, #30) — l'utilisateur ne redimensionne pas
/// encore la boîte lui-même, voir la limitation documentée dans STATUS.md.
const NEW_TEXT_BOX_SIZE: (f64, f64) = (220.0, 24.0);
/// Demi-côté (pixels écran) d'une poignée de redimensionnement d'annotation
/// (#32) — même valeur pour la zone dessinée et la zone cliquable/glissable.
const ANNOTATION_HANDLE_HALF: f32 = 5.0;
/// Taille minimale (points page) qu'un redimensionnement d'annotation peut
/// atteindre — évite un rect dégénéré (largeur/hauteur nulle ou négative) si
/// l'utilisateur glisse une poignée au-delà du coin opposé.
const ANNOTATION_MIN_SIZE: f64 = 4.0;

/// Un coin du rectangle d'une annotation sélectionnée (#32) — celui que
/// l'utilisateur a saisi pour redimensionner par glissement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Glissement en cours sur `selected_annotation` (#32) — déplacement
/// (translation, taille inchangée) ou redimensionnement par un coin. Les deux
/// capturent le rect *au début* du glissement (`start_rect`) et la position
/// écran de départ (`start_pointer`) : le rect affiché à chaque frame est
/// recalculé depuis ce point de départ plutôt qu'accumulé delta par delta,
/// pour éviter toute dérive numérique sur un glissement long.
#[derive(Debug, Clone, Copy)]
enum AnnotationDrag {
    Move {
        start_pointer: egui::Pos2,
        start_rect: [f64; 4],
    },
    Resize {
        corner: Corner,
        start_pointer: egui::Pos2,
        start_rect: [f64; 4],
    },
}

/// Ce que fait `text_modal` une fois l'utilisateur ayant validé sa saisie
/// (Sprint 20) — `AddFreeText` pour #30 (ajouter du texte), `ReplaceText`
/// pour #40 (remplacer un texte existant par superposition, réutilise la
/// sélection courante).
enum PendingTextAction {
    AddFreeText {
        rect: [f64; 4],
    },
    ReplaceText {
        rect: [f64; 4],
    },
    /// Remplir un champ de formulaire texte au clic (Sprint 23) — la
    /// modale est préremplie avec la valeur actuelle du champ plutôt que
    /// vide, contrairement aux deux autres variantes.
    FillForm {
        field_name: String,
    },
}

/// État de la boîte de dialogue modale de saisie de texte (Sprint 20) —
/// une seule à la fois, partagée entre "ajouter du texte" et "remplacer le
/// texte sélectionné" (`action` distingue laquelle des deux valider).
struct TextInputModal {
    action: PendingTextAction,
    input: String,
}

/// État de la fenêtre de réglage couleur/opacité d'une annotation (#32,
/// Sprint 55) — préremplie avec la couleur actuelle de l'annotation (voir
/// `pdf_app::AnnotationInfo::color`) et une opacité par défaut de 1.0 (pas
/// lue depuis `/AP`, qui n'est pas exposé jusqu'ici — un réglage explicite
/// de l'opacité écrase donc toujours la valeur précédente, y compris
/// `HIGHLIGHT_FILL_ALPHA`).
struct AnnotationStylePopup {
    annot_index: usize,
    color: [f32; 3],
    opacity: f32,
}

fn main() -> eframe::Result<()> {
    // Backend `wgpu` plutôt que le `glow` par défaut d'eframe : condition
    // nécessaire pour partager le `Device`/`Queue` d'eframe avec
    // `pdf-render-gpu` (voir `ViewerApp::new` et
    // `pdf_render_gpu::GpuRenderer::from_shared`) — sans quoi ce backend
    // devrait renégocier son propre device à chaque page (voir la doc de
    // module de `pdf-render-gpu`, le problème que cette intégration résout).
    // Icône de fenêtre/dock en `cargo run` (hors bundle `.app`, qui utilise
    // `icons/icon.icns` via `[package.metadata.bundle]`, voir Cargo.toml) :
    // sans elle, `eframe` affiche l'icône Rust générique par défaut.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../icons/icon_256.png"))
        .expect("icône embarquée invalide");
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "PapyrusPDF",
        options,
        Box::new(|cc| {
            let gpu = cc.wgpu_render_state.as_ref().map(|rs| {
                pdf_render_gpu::GpuRenderer::from_shared(rs.device.clone(), rs.queue.clone())
            });
            // La barre de menus native n'est **pas** installée ici : ce
            // callback tourne avant que la boucle d'événements `winit`
            // démarre réellement, et `winit` installe son propre menu par
            // défaut (juste "Quitter") au démarrage de cette boucle — après
            // ce callback. L'installer ici risquait donc de se faire
            // silencieusement écraser. `ViewerApp::update` l'installe plutôt
            // à sa toute première frame, garantie de tourner une fois la
            // boucle `winit`/AppKit pleinement démarrée.
            Ok(Box::new(ViewerApp::new(
                std::env::args().nth(1).map(PathBuf::from),
                gpu,
            )))
        }),
    )
}

/// Résultat de `DocumentTab::update_content` transmis à `ViewerApp` — pour
/// l'instant, seule l'ouverture d'un nouveau document (bouton "Ouvrir…" de
/// la barre d'outils d'un onglet) doit remonter au niveau de l'application
/// (elle crée un **nouvel onglet**, elle ne remplace jamais le contenu de
/// l'onglet courant — voir la doc de module).
#[derive(Default)]
struct DocumentTabOutcome {
    open_in_new_tab: Option<PathBuf>,
}

/// État complet d'un document ouvert dans un onglet (Sprint 49) — tout ce
/// qui était auparavant un champ direct de `ViewerApp` avant l'introduction
/// des onglets multi-documents, à l'exception du backend GPU partagé (cloné
/// une fois à la création, voir `gpu`) et de la barre de menus native (une
/// seule pour toute l'application, portée par `ViewerApp`).
struct DocumentTab {
    session: Option<Session>,
    zoom: f32,
    /// Mis à `true` par le bouton "Ajuster à la largeur" ; consommé au
    /// prochain rendu du `CentralPanel` (mode page unique), seul endroit où
    /// la largeur disponible réelle (`ui.available_width()`) est connue.
    fit_width_requested: bool,
    /// Comme `fit_width_requested`, pour le bouton "↕ Ajuster à la page"
    /// (Sprint 21, #10) — consommé au prochain rendu du `CentralPanel`, où
    /// `ui.available_size()` (largeur **et** hauteur) est connue.
    fit_page_requested: bool,
    texture: Option<egui::TextureHandle>,
    /// (page affichée, zoom affiché, densité de pixels affichée) par la
    /// texture courante — sert à détecter qu'un nouveau rendu est
    /// nécessaire sans re-rasteriser à chaque frame. La densité
    /// (`ctx.pixels_per_point()`) fait partie de la clé : sans elle, une
    /// page déjà rendue à 1x resterait floue si la fenêtre passe sur un
    /// écran Retina (2x) sans que rien d'autre n'ait changé.
    texture_state: Option<(usize, u32, u32)>,
    /// Taille d'affichage (points logiques `egui`, PAS pixels de texture)
    /// de `texture` — nécessaire parce que la texture est désormais rendue
    /// à `zoom * pixels_per_point` pixels physiques pour rester nette sur
    /// écran Retina ; sans forcer explicitement la taille d'affichage,
    /// `egui::Image` afficherait la texture à sa taille en pixels
    /// interprétée comme des points, deux fois trop grande sur un écran 2x.
    texture_logical_size: Option<egui::Vec2>,
    error: Option<String>,
    /// Contenu du champ "Aller à la page" (Sprint 18) — texte libre plutôt
    /// que déjà parsé, pour permettre à l'utilisateur de taper sans que le
    /// champ ne se corrige/reformatte sous ses yeux à chaque frappe.
    goto_page_input: String,
    search_query: String,
    /// Index des pages (0-based) contenant `search_query`, dans l'ordre du
    /// document ; `None` tant qu'aucune recherche n'a été lancée.
    search_results: Option<Vec<usize>>,
    /// Position courante dans `search_results` pour "occurrence suivante".
    search_cursor: usize,
    /// Requête effectivement recherchée (fige la valeur de `search_query`
    /// au moment de `run_search`, pour que le surlignage ne change pas
    /// pendant que l'utilisateur retape dans le champ).
    highlighted_query: String,
    /// (page affichée, requête surlignée) pour laquelle `highlight_rects`
    /// est à jour — évite de refaire la recherche de position à chaque frame.
    highlight_state: Option<(usize, String)>,
    highlight_rects: Vec<pdf_app::MatchRect>,
    show_thumbnails: bool,
    thumbnails: HashMap<usize, egui::TextureHandle>,
    /// Pages cochées dans le panneau de miniatures (Sprint 19, manipulation
    /// de pages) — sert uniquement à "✂ Extraire la sélection…" ; vidé après
    /// toute édition (voir `invalidate_after_edit`) puisqu'une insertion,
    /// suppression ou déplacement de page rend les indices précédents
    /// périmés.
    thumbnail_selection: HashSet<usize>,
    show_outline: bool,
    continuous_scroll: bool,
    /// Textures du mode défilement continu, indépendantes de `texture`
    /// (mode page unique) : une par page déjà rastérisée à l'échelle
    /// courante. Vidées quand le zoom ou la densité de pixels de l'écran
    /// change (voir `page_textures_zoom_key`).
    page_textures: HashMap<usize, egui::TextureHandle>,
    page_textures_zoom_key: Option<(u32, u32)>,
    /// Page à faire défiler jusqu'à l'écran au prochain rendu du mode
    /// continu (navigation depuis la recherche/miniatures/signets/boutons) ;
    /// consommé (mis à `None`) une fois appliqué pour ne pas entraver le
    /// défilement manuel de l'utilisateur ensuite.
    scroll_to_page: Option<usize>,
    /// Page sur laquelle porte la sélection de texte courante — sert à
    /// l'invalider si l'utilisateur change de page sans avoir relâché de
    /// sélection (les indices de caractères n'ont de sens que par page).
    selection_page: Option<usize>,
    /// Indice de caractère où le glissement de sélection a commencé.
    selection_anchor: Option<usize>,
    /// Indice de caractère sous le pointeur pendant/après le glissement.
    selection_cursor: Option<usize>,
    selection_rects: Vec<pdf_app::MatchRect>,
    selection_text: String,
    /// Armé par le bouton "📝 Ajouter texte" (Sprint 20, #30) : le prochain
    /// simple clic (pas glissement) sur la page ouvre `text_modal` pour
    /// saisir le texte à ajouter à cet endroit, puis se désarme.
    add_text_mode: bool,
    /// Boîte de dialogue de saisie de texte actuellement ouverte (#30/#40),
    /// `None` si aucune — voir `TextInputModal`.
    text_modal: Option<TextInputModal>,
    /// Annotations de la page courante (Sprint 20, #32) — recalculées à
    /// chaque changement de page/édition (voir `ensure_annotations`),
    /// affichées en contour cliquable pour la suppression.
    annotations: Vec<pdf_app::AnnotationInfo>,
    /// (page affichée) pour laquelle `annotations` est à jour — évite de
    /// rappeler `Session::annotations_on_current_page` à chaque frame.
    annotations_state: Option<usize>,
    /// Annotation actuellement sélectionnée dans le contour cliquable
    /// (#32) — un bouton "🗑 Supprimer l'annotation" apparaît tant qu'une
    /// sélection existe.
    selected_annotation: Option<usize>,
    /// Glissement en cours (déplacement ou redimensionnement par poignée) de
    /// `selected_annotation` (#32) — `None` hors glissement. Voir
    /// `handle_annotation_drag`.
    annotation_drag: Option<AnnotationDrag>,
    /// Rect "en direct" de l'annotation en cours de glissement, recalculé à
    /// chaque frame de `dragged()` sans toucher `pdf-edit` (aucun `EditOp`
    /// par frame) — seul `drag_stopped()` commet le rect final via
    /// `Session::set_annotation_rect_on_current_page`. `None` hors
    /// glissement, auquel cas `draw_annotation_outlines` retombe sur
    /// `annotations[i].rect`.
    annotation_drag_preview: Option<[f64; 4]>,
    /// Champs de formulaire texte de la page courante — recalculés à
    /// chaque changement de page/édition (voir `ensure_form_fields`, même
    /// schéma que `annotations`/`ensure_annotations`), affichés en contour
    /// cliquable pour ouvrir `text_modal` et saisir une nouvelle valeur.
    form_fields: Vec<pdf_edit::FormFieldInfo>,
    /// (page affichée) pour laquelle `form_fields` est à jour.
    form_fields_state: Option<usize>,
    /// Cases à cocher de la page courante (Sprint 52, #43 suite) — même
    /// schéma que `form_fields`/`ensure_form_fields`, mais un clic bascule
    /// directement l'état (pas de modale de saisie, contrairement au texte).
    checkbox_fields: Vec<pdf_edit::CheckboxFieldInfo>,
    /// (page affichée) pour laquelle `checkbox_fields` est à jour.
    checkbox_fields_state: Option<usize>,
    /// Groupes de boutons radio de la page courante (Sprint 53, #43 suite) —
    /// même schéma que `checkbox_fields`, un clic sur une option sélectionne
    /// directement (pas de modale).
    radio_groups: Vec<pdf_edit::RadioGroupInfo>,
    /// (page affichée) pour laquelle `radio_groups` est à jour.
    radio_groups_state: Option<usize>,
    /// Champs liste/menu déroulant de la page courante (Sprint 54, #43
    /// suite, dernier sous-cas) — contrairement à une case à cocher/un
    /// bouton radio, une seule zone cliquable par champ (pas une par
    /// option) : un clic ouvre `choice_field_popup` plutôt que de basculer
    /// directement un état.
    choice_fields: Vec<pdf_edit::ChoiceFieldInfo>,
    /// (page affichée) pour laquelle `choice_fields` est à jour.
    choice_fields_state: Option<usize>,
    /// Nom du champ liste/menu déroulant dont la fenêtre de sélection
    /// d'option est actuellement ouverte (`None` si aucune) — voir
    /// `handle_choice_field_click`/`show_choice_field_popup`.
    choice_field_popup: Option<String>,
    /// Fenêtre de réglage couleur/opacité ouverte par le bouton "🎨
    /// Style…" (#32, Sprint 55) — `None` si aucune. `annot_index` cible
    /// l'indice dans `self.annotations`, `color`/`opacity` sont éditées en
    /// direct dans la fenêtre puis appliquées via
    /// `Session::set_annotation_style_on_current_page`.
    annotation_style_popup: Option<AnnotationStylePopup>,
    /// Dernier décalage de défilement connu de la `ScrollArea` de la page
    /// unique (Sprint 21, #9 — pincement centré sur le curseur) — mis à jour
    /// à chaque frame depuis `ScrollAreaOutput::state.offset`, réutilisé au
    /// prochain pincement pour calculer le nouveau décalage sans dépendre
    /// d'un accès direct à l'état interne de `ScrollArea`.
    last_scroll_offset: egui::Vec2,
    /// Rectangle écran de la zone de défilement à la frame précédente — sert
    /// à convertir la position du curseur en coordonnées locales à la zone,
    /// avec un décalage d'une frame (imperceptible, la taille de la fenêtre
    /// ne change pas à chaque frame).
    last_scroll_viewport: Option<egui::Rect>,
    /// Décalage à appliquer à la `ScrollArea` à la prochaine frame (calculé
    /// par un pincement, consommé par `ScrollArea::scroll_offset` avant le
    /// prochain rendu).
    pending_scroll_offset: Option<egui::Vec2>,
    /// Compteur d'impressions demandées dans cet onglet (Sprint 21, #48) —
    /// sert uniquement à donner un nom de fichier temporaire unique par
    /// impression, voir `print_document`.
    print_requests: u64,
    /// Thread de rendu en arrière-plan (Sprint 21, #20) — miniatures et
    /// pages du défilement continu, voir `render_worker`. Un par onglet :
    /// chaque `DocumentTab` a son propre document, indépendant des autres.
    render_worker: RenderWorker,
    /// Incrémenté à l'ouverture d'un fichier et après chaque édition (voir
    /// `refresh_render_worker_document`) : les résultats du thread
    /// d'arrière-plan portant une autre génération sont ignorés
    /// (`drain_background_renders`), et les requêtes déjà envoyées pour
    /// l'ancienne génération n'écrasent jamais un résultat plus récent.
    render_generation: u64,
    /// `(kind, page_index, scale_key)` déjà envoyés à `render_worker` pour
    /// `render_generation` — évite de renvoyer la même requête à chaque
    /// frame tant que le résultat n'est pas encore arrivé. Vidé à chaque
    /// nouvelle génération (voir `refresh_render_worker_document`).
    background_requested: HashSet<(RenderKind, usize, u32)>,
    /// `Device`/`Queue` partagés avec le renderer `wgpu` d'eframe (voir
    /// `main`) — `None` si le backend `glow` a été sélectionné ou si aucun
    /// adaptateur `wgpu` n'était disponible au démarrage. Cloné depuis
    /// `ViewerApp::gpu` à la création de l'onglet (voir `ViewerApp::open_new_tab`) :
    /// `GpuRenderer` ne fait que partager des `Arc`, un clone est donc bon
    /// marché.
    gpu: Option<pdf_render_gpu::GpuRenderer>,
}

impl DocumentTab {
    fn new(initial_path: Option<PathBuf>, gpu: Option<pdf_render_gpu::GpuRenderer>) -> Self {
        let mut tab = Self {
            session: None,
            gpu,
            zoom: 1.0,
            fit_width_requested: false,
            fit_page_requested: false,
            texture: None,
            texture_state: None,
            texture_logical_size: None,
            error: None,
            goto_page_input: String::new(),
            search_query: String::new(),
            search_results: None,
            search_cursor: 0,
            highlighted_query: String::new(),
            highlight_state: None,
            highlight_rects: Vec::new(),
            show_thumbnails: false,
            thumbnails: HashMap::new(),
            thumbnail_selection: HashSet::new(),
            show_outline: false,
            continuous_scroll: false,
            page_textures: HashMap::new(),
            page_textures_zoom_key: None,
            scroll_to_page: None,
            selection_page: None,
            selection_anchor: None,
            selection_cursor: None,
            selection_rects: Vec::new(),
            selection_text: String::new(),
            add_text_mode: false,
            text_modal: None,
            annotations: Vec::new(),
            annotations_state: None,
            selected_annotation: None,
            annotation_drag: None,
            annotation_drag_preview: None,
            form_fields: Vec::new(),
            form_fields_state: None,
            checkbox_fields: Vec::new(),
            checkbox_fields_state: None,
            radio_groups: Vec::new(),
            radio_groups_state: None,
            choice_fields: Vec::new(),
            choice_fields_state: None,
            choice_field_popup: None,
            annotation_style_popup: None,
            last_scroll_offset: egui::Vec2::ZERO,
            last_scroll_viewport: None,
            pending_scroll_offset: None,
            print_requests: 0,
            render_worker: RenderWorker::spawn(),
            render_generation: 0,
            background_requested: HashSet::new(),
        };
        if let Some(p) = initial_path {
            tab.open_path(p);
        }
        tab
    }

    /// Titre affiché dans la barre d'onglets (`ViewerApp::update`) — le nom
    /// de fichier du document ouvert, ou un intitulé générique pour un
    /// onglet encore vide (juste après "Fermer l'onglet" sur le dernier
    /// onglet restant, voir `ViewerApp::close_tab`).
    fn title(&self) -> String {
        match &self.session {
            Some(session) => session
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Document".to_string()),
            None => "Nouvel onglet".to_string(),
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        self.error = None;
        self.texture = None;
        self.texture_state = None;
        self.texture_logical_size = None;
        self.search_results = None;
        self.search_cursor = 0;
        self.highlighted_query.clear();
        self.highlight_state = None;
        self.highlight_rects.clear();
        self.thumbnails.clear();
        self.thumbnail_selection.clear();
        self.page_textures.clear();
        self.page_textures_zoom_key = None;
        self.scroll_to_page = None;
        self.selection_page = None;
        self.selection_anchor = None;
        self.selection_cursor = None;
        self.selection_rects.clear();
        self.selection_text.clear();
        self.add_text_mode = false;
        self.text_modal = None;
        self.annotations.clear();
        self.annotations_state = None;
        self.selected_annotation = None;
        self.annotation_drag = None;
        self.annotation_drag_preview = None;
        self.annotation_style_popup = None;
        self.last_scroll_offset = egui::Vec2::ZERO;
        self.last_scroll_viewport = None;
        self.pending_scroll_offset = None;

        match Session::open(path) {
            Ok(mut session) => {
                if let Some(gpu) = &self.gpu {
                    session.set_gpu_renderer(gpu.clone());
                }
                self.session = Some(session);
                self.refresh_render_worker_document();
            }
            Err(e) => {
                self.error = Some(format!("Impossible d'ouvrir le fichier : {e}"));
                self.session = None;
            }
        }
    }

    /// "Fichier > Exporter une copie…" (`⇧⌘S`) : copie le fichier
    /// actuellement ouvert **sur disque** vers un nouvel emplacement choisi
    /// via `NSSavePanel` (`rfd`) — une copie brute du fichier d'origine, pas
    /// des modifications en attente dans la session d'édition (utiliser
    /// "Enregistrer", `⌘S`, pour celles-ci — voir `save_in_place`).
    fn export_copy_as(&mut self) {
        let Some(session) = &self.session else {
            self.error = Some("Aucun document ouvert à exporter.".to_string());
            return;
        };
        let source = session.path().to_path_buf();
        let default_name = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "document.pdf".to_string());
        if let Some(dest) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(&default_name)
            .save_file()
        {
            if let Err(e) = std::fs::copy(&source, &dest) {
                self.error = Some(format!("Échec de l'export : {e}"));
            }
        }
    }

    /// "🗜 Optimiser et enregistrer sous…" (Sprint 22, #45 — jusqu'ici
    /// seulement accessible via `pdf-cli optimize`) : reconstruit le
    /// document en ne gardant que les objets atteignables
    /// (`Session::export_optimized`, garbage collector par reconstruction —
    /// nettoie en particulier les objets orphelins laissés par `undo`) et
    /// écrit le résultat vers un nouvel emplacement choisi via `NSSavePanel`.
    /// N'écrase jamais le fichier ouvert : c'est délibérément un "enregistrer
    /// sous", pas un "enregistrer" (l'optimisation réécrit tout le fichier,
    /// contrairement à la sauvegarde incrémentale habituelle).
    fn export_optimized_dialog(&mut self) {
        let Some(session) = &self.session else {
            self.error = Some("Aucun document ouvert à optimiser.".to_string());
            return;
        };
        let bytes = match session.export_optimized() {
            Ok(bytes) => bytes,
            Err(e) => {
                self.error = Some(format!("Échec de l'optimisation : {e}"));
                return;
            }
        };
        let default_name = session
            .path()
            .file_stem()
            .map(|n| format!("{}-optimise.pdf", n.to_string_lossy()))
            .unwrap_or_else(|| "document-optimise.pdf".to_string());
        if let Some(dest) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(&default_name)
            .save_file()
        {
            if let Err(e) = std::fs::write(&dest, bytes) {
                self.error = Some(format!("Échec de l'export optimisé : {e}"));
            }
        }
    }

    /// Exporte le texte de tout le document vers un fichier `.txt` choisi via
    /// `NSSavePanel` (`rfd`) — Sprint 18. Réutilise `Session::extract_all_text`
    /// (une page par section séparée par un saut de page), pas de nouvelle
    /// extraction : le texte de chaque page est déjà mis en cache par
    /// `pdf-app` dès qu'il a été vu une première fois (recherche, affichage).
    fn export_text_dialog(&mut self) {
        let Some(session) = &self.session else {
            self.error = Some("Aucun document ouvert à exporter.".to_string());
            return;
        };
        let text = match session.extract_all_text() {
            Ok(text) => text,
            Err(e) => {
                self.error = Some(format!("Échec de l'extraction du texte : {e}"));
                return;
            }
        };
        let default_name = session
            .path()
            .file_stem()
            .map(|n| format!("{}.txt", n.to_string_lossy()))
            .unwrap_or_else(|| "document.txt".to_string());
        if let Some(dest) = rfd::FileDialog::new()
            .add_filter("Texte", &["txt"])
            .set_file_name(&default_name)
            .save_file()
        {
            if let Err(e) = std::fs::write(&dest, text) {
                self.error = Some(format!("Échec de l'export du texte : {e}"));
            }
        }
    }

    /// "Fichier > Enregistrer" (`⌘S`, Sprints 13-17) : écrit les
    /// modifications en attente (annotations, formulaires, pages...) dans le
    /// fichier réellement ouvert, par sauvegarde incrémentale
    /// (`pdf_app::Session::save_edits_in_place`).
    fn save_in_place(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        if let Err(e) = session.save_edits_in_place() {
            self.error = Some(format!("Échec de l'enregistrement : {e}"));
        }
    }

    /// "Fichier > Imprimer…" (`⌘P`, Sprint 21, #48) : écrit le document
    /// courant (éditions en attente incluses, `Session::current_bytes`) dans
    /// un fichier temporaire, puis délègue l'impression à Aperçu via
    /// AppleScript (`osascript`) — donne gratuitement l'aperçu et la
    /// sélection de pages du système, sans construire un pipeline
    /// `NSPrintOperation` maison (cette app n'a pas de vraie `NSView` de
    /// contenu, tout passe par une texture `egui`/`wgpu`). Le fichier
    /// temporaire n'est jamais nettoyé explicitement : `TMPDIR` est vidé par
    /// le système, et Aperçu garde le descripteur ouvert le temps de
    /// l'utiliser.
    fn print_document(&mut self) {
        let Some(session) = &self.session else {
            self.error = Some("Aucun document ouvert à imprimer.".to_string());
            return;
        };
        let bytes = match session.current_bytes() {
            Ok(bytes) => bytes,
            Err(e) => {
                self.error = Some(format!("Impossible de préparer l'impression : {e}"));
                return;
            }
        };
        let temp_path = std::env::temp_dir().join(format!(
            "papyruspdf_print_{}_{}.pdf",
            std::process::id(),
            self.print_request_count()
        ));
        if let Err(e) = std::fs::write(&temp_path, bytes) {
            self.error = Some(format!("Impossible de préparer l'impression : {e}"));
            return;
        }
        let path_str = temp_path.display().to_string();
        let script = format!(
            "tell application \"Preview\"\n\
             activate\n\
             open POSIX file \"{path_str}\"\n\
             print (POSIX file \"{path_str}\") with properties {{print dialog:true}}\n\
             end tell"
        );
        if let Err(e) = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn()
        {
            self.error = Some(format!("Impossible de lancer l'impression : {e}"));
        }
    }

    /// Suffixe unique pour le nom du fichier temporaire d'impression — sinon
    /// deux impressions successives dans le même onglet réutiliseraient le
    /// même chemin, d'où un fichier périmé si Aperçu l'a encore ouvert.
    fn print_request_count(&mut self) -> u64 {
        self.print_requests += 1;
        self.print_requests
    }

    /// Défait la dernière opération d'édition et invalide tout ce qui
    /// dépend du contenu de la page (texture affichée, miniatures,
    /// surlignage de recherche).
    fn undo_edit(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.undo_edit() {
            Ok(true) => self.invalidate_after_edit(),
            Ok(false) => {}
            Err(e) => self.error = Some(format!("Impossible d'annuler : {e}")),
        }
    }

    fn redo_edit(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.redo_edit() {
            Ok(true) => self.invalidate_after_edit(),
            Ok(false) => {}
            Err(e) => self.error = Some(format!("Impossible de rétablir : {e}")),
        }
    }

    /// Surligne la sélection de texte courante (`self.selection_rects`) —
    /// un seul rectangle `/Highlight` couvrant leur boîte englobante plutôt
    /// qu'un par ligne, plus simple et suffisant pour une sélection sur une
    /// seule page (voir la limitation connue : pas de sélection en mode
    /// défilement continu).
    fn highlight_selection(&mut self) {
        let Some(rect) = self.selection_bbox() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        match session.add_highlight_on_current_page(rect, (1.0, 1.0, 0.0)) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de surligner : {e}")),
        }
    }

    /// Souligne la sélection de texte courante (Sprint 20, #26).
    fn underline_selection(&mut self) {
        let Some(rect) = self.selection_bbox() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        match session.add_underline_on_current_page(rect, (1.0, 0.0, 0.0)) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de souligner : {e}")),
        }
    }

    /// Barre la sélection de texte courante (Sprint 20, #26).
    fn strikeout_selection(&mut self) {
        let Some(rect) = self.selection_bbox() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        match session.add_strikeout_on_current_page(rect, (1.0, 0.0, 0.0)) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de barrer : {e}")),
        }
    }

    /// Boîte englobante (espace page PDF) de `self.selection_rects` — partagé
    /// par le surlignage, le soulignement et le barré, qui n'ont besoin que
    /// d'un seul rectangle couvrant la sélection plutôt que d'un par ligne
    /// (voir la limitation connue : pas de sélection en mode défilement
    /// continu, donc toujours une seule page).
    fn selection_bbox(&self) -> Option<[f64; 4]> {
        if self.selection_rects.is_empty() {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for rect in &self.selection_rects {
            x0 = x0.min(rect.x0);
            y0 = y0.min(rect.y0);
            x1 = x1.max(rect.x1);
            y1 = y1.max(rect.y1);
        }
        Some([x0, y0, x1, y1])
    }

    /// Ouvre la boîte de dialogue de saisie pour remplacer (superposition,
    /// #40) la sélection de texte courante par un nouveau texte — préremplit
    /// le champ avec le texte actuellement sélectionné, à éditer sur place.
    fn open_replace_text_modal(&mut self) {
        let Some(rect) = self.selection_bbox() else {
            return;
        };
        self.text_modal = Some(TextInputModal {
            action: PendingTextAction::ReplaceText { rect },
            input: self.selection_text.clone(),
        });
    }

    /// Confirme `self.text_modal` (bouton "OK"/`Enter`) : ajoute le texte
    /// libre (#30) ou remplace le texte de la sélection (#40) selon
    /// `PendingTextAction`, puis ferme la boîte de dialogue.
    fn confirm_text_modal(&mut self) {
        let Some(modal) = self.text_modal.take() else {
            return;
        };
        if modal.input.trim().is_empty() {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        let result = match modal.action {
            PendingTextAction::AddFreeText { rect } => session.add_free_text_on_current_page(
                rect,
                &modal.input,
                DEFAULT_TEXT_FONT_SIZE,
                (0.0, 0.0, 0.0),
            ),
            PendingTextAction::ReplaceText { rect } => session.replace_text_on_current_page(
                rect,
                &modal.input,
                DEFAULT_TEXT_FONT_SIZE,
                (0.0, 0.0, 0.0),
                (1.0, 1.0, 1.0),
            ),
            PendingTextAction::FillForm { ref field_name } => {
                session.set_form_field_value_on_current_page(field_name, &modal.input)
            }
        };
        match result {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible d'appliquer le texte : {e}")),
        }
    }

    /// Recalcule `self.annotations` si la page affichée a changé depuis le
    /// dernier appel (Sprint 20, #32) — même schéma que `ensure_highlights`.
    fn ensure_annotations(&mut self) {
        let Some(session) = &self.session else {
            self.annotations.clear();
            return;
        };
        let page_index = session.page_index();
        if self.annotations_state == Some(page_index) {
            return;
        }
        self.annotations = session.annotations_on_current_page().unwrap_or_default();
        self.annotations_state = Some(page_index);
    }

    /// Recalcule `self.form_fields` si la page affichée a changé depuis le
    /// dernier appel — même schéma que `ensure_annotations`.
    fn ensure_form_fields(&mut self) {
        let Some(session) = &self.session else {
            self.form_fields.clear();
            return;
        };
        let page_index = session.page_index();
        if self.form_fields_state == Some(page_index) {
            return;
        }
        self.form_fields = session.form_fields_on_current_page().unwrap_or_default();
        self.form_fields_state = Some(page_index);
    }

    /// Recalcule `self.checkbox_fields` si la page affichée a changé depuis
    /// le dernier appel — même schéma que `ensure_form_fields` (Sprint 52).
    fn ensure_checkbox_fields(&mut self) {
        let Some(session) = &self.session else {
            self.checkbox_fields.clear();
            return;
        };
        let page_index = session.page_index();
        if self.checkbox_fields_state == Some(page_index) {
            return;
        }
        self.checkbox_fields = session
            .checkbox_fields_on_current_page()
            .unwrap_or_default();
        self.checkbox_fields_state = Some(page_index);
    }

    /// Bascule (coché <-> décoché) la case de `self.checkbox_fields` dont le
    /// rect contient le prochain simple clic sur la page (Sprint 52, #43
    /// suite) — contrairement à un champ texte, pas de modale de saisie :
    /// l'action est immédiate. Renvoie `true` si le clic est tombé dans une
    /// case (consommé), pour que l'appelant ne le traite pas aussi comme une
    /// sélection/un clic d'annotation.
    fn handle_checkbox_field_click(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) -> bool {
        if !response.clicked() {
            return false;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };
        let (px, py) = screen_to_page(pos, response.rect, media_box, scale);
        let Some(field) = self
            .checkbox_fields
            .iter()
            .find(|f| px >= f.rect[0] && px <= f.rect[2] && py >= f.rect[1] && py <= f.rect[3])
        else {
            return false;
        };
        let name = field.name.clone();
        let new_checked = !field.checked;
        let Some(session) = &mut self.session else {
            return true;
        };
        match session.set_checkbox_field_value_on_current_page(&name, new_checked) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de cocher/décocher le champ : {e}")),
        }
        true
    }

    /// Recalcule `self.radio_groups` si la page affichée a changé depuis le
    /// dernier appel — même schéma que `ensure_checkbox_fields` (Sprint 53).
    fn ensure_radio_groups(&mut self) {
        let Some(session) = &self.session else {
            self.radio_groups.clear();
            return;
        };
        let page_index = session.page_index();
        if self.radio_groups_state == Some(page_index) {
            return;
        }
        self.radio_groups = session.radio_groups_on_current_page().unwrap_or_default();
        self.radio_groups_state = Some(page_index);
    }

    /// Sélectionne l'option de `self.radio_groups` dont le rect contient le
    /// prochain simple clic sur la page (Sprint 53, #43 suite) — même schéma
    /// que `handle_checkbox_field_click`, mais bascule une option parmi
    /// plusieurs plutôt que coché/décoché.
    fn handle_radio_group_click(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) -> bool {
        if !response.clicked() {
            return false;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };
        let (px, py) = screen_to_page(pos, response.rect, media_box, scale);
        let hit = self.radio_groups.iter().find_map(|group| {
            group
                .options
                .iter()
                .position(|o| {
                    px >= o.rect[0] && px <= o.rect[2] && py >= o.rect[1] && py <= o.rect[3]
                })
                .map(|option_index| (group.name.clone(), option_index))
        });
        let Some((name, option_index)) = hit else {
            return false;
        };
        let Some(session) = &mut self.session else {
            return true;
        };
        match session.set_radio_group_value_on_current_page(&name, option_index) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de sélectionner l'option : {e}")),
        }
        true
    }

    /// Recalcule `self.choice_fields` si la page affichée a changé depuis le
    /// dernier appel — même schéma que `ensure_radio_groups` (Sprint 54).
    fn ensure_choice_fields(&mut self) {
        let Some(session) = &self.session else {
            self.choice_fields.clear();
            return;
        };
        let page_index = session.page_index();
        if self.choice_fields_state == Some(page_index) {
            return;
        }
        self.choice_fields = session.choice_fields_on_current_page().unwrap_or_default();
        self.choice_fields_state = Some(page_index);
    }

    /// Ouvre `self.choice_field_popup` pour le champ de `self.choice_fields`
    /// dont le rect contient le prochain simple clic sur la page (Sprint 54,
    /// #43 suite) — contrairement à une case à cocher/un bouton radio,
    /// une seule zone cliquable par champ propose une liste d'options
    /// plutôt que de basculer directement un état (voir
    /// `show_choice_field_popup`).
    fn handle_choice_field_click(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) -> bool {
        if !response.clicked() {
            return false;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };
        let (px, py) = screen_to_page(pos, response.rect, media_box, scale);
        let Some(field) = self
            .choice_fields
            .iter()
            .find(|f| px >= f.rect[0] && px <= f.rect[2] && py >= f.rect[1] && py <= f.rect[3])
        else {
            return false;
        };
        self.choice_field_popup = Some(field.name.clone());
        true
    }

    /// Affiche la fenêtre de sélection d'option ouverte par
    /// `handle_choice_field_click` (Sprint 54, #43 suite), le cas échéant —
    /// une option par bouton, cliquer une la sélectionne immédiatement et
    /// ferme la fenêtre (pas de bouton "Valider" séparé).
    fn show_choice_field_popup(&mut self, ctx: &egui::Context) {
        let Some(field_name) = self.choice_field_popup.clone() else {
            return;
        };
        let Some(field) = self
            .choice_fields
            .iter()
            .find(|f| f.name == field_name)
            .cloned()
        else {
            self.choice_field_popup = None;
            return;
        };

        let mut close = false;
        let mut chosen: Option<usize> = None;
        egui::Window::new(format!("Choisir : {field_name}"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                for (index, option) in field.options.iter().enumerate() {
                    let selected = field.selected_index == Some(index);
                    if ui.selectable_label(selected, option).clicked() {
                        chosen = Some(index);
                    }
                }
                ui.separator();
                if ui.button("Annuler").clicked() {
                    close = true;
                }
            });

        if let Some(option_index) = chosen {
            if let Some(session) = &mut self.session {
                match session.set_choice_field_value_on_current_page(&field_name, option_index) {
                    Ok(()) => self.invalidate_after_edit(),
                    Err(e) => {
                        self.error = Some(format!("Impossible de sélectionner l'option : {e}"))
                    }
                }
            }
            close = true;
        }
        if close {
            self.choice_field_popup = None;
        }
    }

    /// Supprime l'annotation actuellement sélectionnée (`self.selected_annotation`,
    /// bouton "🗑 Supprimer l'annotation", #32).
    fn delete_selected_annotation(&mut self) {
        let Some(index) = self.selected_annotation.take() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        match session.remove_annotation_on_current_page(index) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de supprimer l'annotation : {e}")),
        }
    }

    /// Ouvre `self.annotation_style_popup` pour l'annotation `annot_index`
    /// (bouton "🎨 Style…", #32, Sprint 55) — préremplie avec sa couleur
    /// actuelle (`AnnotationInfo::color`, blanc par défaut si absente) et
    /// une opacité de 1.0 (voir la doc de `AnnotationStylePopup` sur
    /// pourquoi l'opacité n'est pas préremplie depuis le fichier).
    fn open_annotation_style_popup(&mut self, annot_index: usize) {
        let color = self
            .annotations
            .iter()
            .find(|a| a.index == annot_index)
            .and_then(|a| a.color)
            .map(|(r, g, b)| [r, g, b])
            .unwrap_or([1.0, 1.0, 1.0]);
        self.annotation_style_popup = Some(AnnotationStylePopup {
            annot_index,
            color,
            opacity: 1.0,
        });
    }

    /// Affiche la fenêtre ouverte par `open_annotation_style_popup`, le cas
    /// échéant — un sélecteur de couleur (`egui::color_picker`) et un
    /// curseur d'opacité, appliqués immédiatement à chaque changement (pas
    /// de bouton "Appliquer" séparé, cohérent avec le reste de #32 qui
    /// évite les allers-retours).
    fn show_annotation_style_popup(&mut self, ctx: &egui::Context) {
        let Some(popup) = &mut self.annotation_style_popup else {
            return;
        };
        let mut close = false;
        let mut apply = false;
        let (annot_index, mut color, mut opacity) = (popup.annot_index, popup.color, popup.opacity);
        egui::Window::new("Style de l'annotation")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Couleur :");
                    if egui::color_picker::color_edit_button_rgb(ui, &mut color).changed() {
                        apply = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Opacité :");
                    if ui
                        .add(egui::Slider::new(&mut opacity, 0.05..=1.0))
                        .changed()
                    {
                        apply = true;
                    }
                });
                ui.separator();
                if ui.button("Fermer").clicked() {
                    close = true;
                }
            });

        if let Some(popup) = &mut self.annotation_style_popup {
            popup.color = color;
            popup.opacity = opacity;
        }
        if apply {
            let Some(session) = &mut self.session else {
                return;
            };
            let rgb = (color[0], color[1], color[2]);
            if let Err(e) =
                session.set_annotation_style_on_current_page(annot_index, rgb, opacity as f64)
            {
                self.error = Some(format!("Impossible de changer le style : {e}"));
            } else {
                // `invalidate_after_edit` efface `annotation_style_popup` et
                // `selected_annotation` (comme après n'importe quelle
                // édition) — on les rouvre/reséectionne immédiatement pour
                // que l'utilisateur puisse continuer à ajuster sans
                // recliquer l'annotation.
                self.invalidate_after_edit();
                self.selected_annotation = Some(annot_index);
                self.annotation_style_popup = Some(AnnotationStylePopup {
                    annot_index,
                    color,
                    opacity,
                });
            }
        }
        if close {
            self.annotation_style_popup = None;
        }
    }

    /// Insère une page blanche à `at_index` (bouton "＋ Page", panneau
    /// miniatures, Sprint 19).
    fn insert_blank_page(&mut self, at_index: usize) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.insert_blank_page_at(at_index) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible d'insérer la page : {e}")),
        }
    }

    /// Ouvre un sélecteur de fichier JPEG et l'insère comme nouvelle page à
    /// `at_index` (bouton "🖼 Image…", Sprint 19 — voir la limitation connue
    /// de `pdf_edit::EditSession::insert_image_page` : JPEG seulement).
    fn insert_image_page_dialog(&mut self, at_index: usize) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JPEG", &["jpg", "jpeg"])
            .pick_file()
        else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.error = Some(format!("Impossible de lire l'image : {e}"));
                return;
            }
        };
        let Some(session) = &mut self.session else {
            return;
        };
        match session.insert_image_page_at(at_index, &bytes) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible d'insérer l'image : {e}")),
        }
    }

    /// Supprime la page `index` (bouton 🗑 par miniature, Sprint 19).
    fn delete_page(&mut self, index: usize) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.delete_page_at(index) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de supprimer la page : {e}")),
        }
    }

    /// Pivote la page `index` de 90° (bouton ↻ par miniature, Sprint 19) —
    /// rotation persistée (`/Rotate`), distincte d'une rotation de vue.
    fn rotate_page(&mut self, index: usize) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.rotate_page_at(index, 90) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de pivoter la page : {e}")),
        }
    }

    /// Déplace la page `from` vers la position `to` (glisser-déposer des
    /// miniatures, Sprint 19).
    fn move_page(&mut self, from: usize, to: usize) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.move_page(from, to) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de déplacer la page : {e}")),
        }
    }

    /// Ouvre un sélecteur de fichier PDF et le fusionne à la fin du document
    /// courant (bouton "📎 PDF…", Sprint 19).
    fn merge_pdf_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file()
        else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        match session.merge_document_from_path(&path) {
            Ok(()) => self.invalidate_after_edit(),
            Err(e) => self.error = Some(format!("Impossible de fusionner le PDF : {e}")),
        }
    }

    /// Extrait les pages cochées (`self.thumbnail_selection`) vers un
    /// nouveau fichier autonome choisi via `NSSavePanel` (bouton
    /// "✂ Extraire…", Sprint 19) — ne modifie pas la session en cours.
    fn extract_selection_dialog(&mut self) {
        if self.thumbnail_selection.is_empty() {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let mut indices: Vec<usize> = self.thumbnail_selection.iter().copied().collect();
        indices.sort_unstable();
        let Some(dest) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name("extrait.pdf")
            .save_file()
        else {
            return;
        };
        if let Err(e) = session.extract_pages_to_file(&indices, &dest) {
            self.error = Some(format!("Impossible d'extraire les pages : {e}"));
        }
    }

    /// À appeler après toute opération d'édition qui modifie la page
    /// affichée : force la re-rasterisation (le cache de `pdf-app` a déjà
    /// été invalidé côté `Session`, mais `pdf-ui` garde ses propres caches
    /// de texture/miniatures indépendants) et le recalcul du surlignage de
    /// recherche (la position des caractères peut changer si une page a été
    /// insérée/supprimée avant la page courante).
    fn invalidate_after_edit(&mut self) {
        self.texture_state = None;
        self.texture_logical_size = None;
        self.thumbnails.clear();
        // Une insertion/suppression/déplacement de page rend les indices
        // précédemment cochés périmés (Sprint 19) — plus sûr de tout vider
        // que de tenter de les faire suivre.
        self.thumbnail_selection.clear();
        self.page_textures.clear();
        self.highlight_state = None;
        self.selection_anchor = None;
        self.selection_cursor = None;
        self.selection_rects.clear();
        self.selection_text.clear();
        // Les indices d'annotation (`self.annotations`, indexés dans
        // `/Annots`) peuvent changer après n'importe quelle édition — plus
        // sûr de forcer un recalcul (`ensure_annotations`) que de les faire
        // suivre.
        self.annotations_state = None;
        self.selected_annotation = None;
        self.annotation_drag = None;
        self.annotation_drag_preview = None;
        // Une valeur de champ modifiée doit être relue (`ensure_form_fields`)
        // pour préremplir correctement la modale au prochain clic.
        self.form_fields_state = None;
        self.checkbox_fields_state = None;
        self.radio_groups_state = None;
        self.choice_fields_state = None;
        self.choice_field_popup = None;
        self.annotation_style_popup = None;
        self.refresh_render_worker_document();
    }

    /// Envoie les octets à jour du document (éditions en attente incluses)
    /// au thread de rendu en arrière-plan (Sprint 21, #20), avec une
    /// nouvelle génération — à appeler à l'ouverture d'un fichier et après
    /// toute édition, jamais pendant une simple navigation/zoom (qui ne
    /// change pas le contenu du document).
    fn refresh_render_worker_document(&mut self) {
        self.render_generation += 1;
        self.background_requested.clear();
        if let Some(session) = &self.session {
            if let Ok(bytes) = session.current_bytes() {
                self.render_worker
                    .set_document(bytes, self.render_generation);
            }
        }
    }

    /// Récupère les pages rastérisées en arrière-plan depuis la dernière
    /// frame (Sprint 21, #20) et les installe dans le cache correspondant
    /// (`thumbnails` ou `page_textures` selon `RenderKind`) — à appeler une
    /// fois par frame, avant de dessiner miniatures/défilement continu.
    /// Ignore silencieusement tout résultat d'une génération périmée (déjà
    /// remplacée par une édition depuis l'envoi de la requête).
    fn drain_background_renders(&mut self, ctx: &egui::Context) {
        for page in self.render_worker.drain_results() {
            if page.generation != self.render_generation {
                continue;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [page.width as usize, page.height as usize],
                &page.rgba,
            );
            let texture = ctx.load_texture(
                format!("bg-{:?}-{}-{}", page.kind, page.page_index, page.scale_key),
                image,
                egui::TextureOptions::LINEAR,
            );
            match page.kind {
                RenderKind::Thumbnail => {
                    self.thumbnails.insert(page.page_index, texture);
                }
                RenderKind::ContinuousPage => {
                    self.page_textures.insert(page.page_index, texture);
                }
            }
        }
    }

    /// Lance une recherche plein document et saute directement à la première
    /// page trouvée (s'il y en a une).
    fn run_search(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.find_pages_containing(&self.search_query) {
            Ok(results) => {
                self.search_cursor = 0;
                if let Some(&first) = results.first() {
                    session.goto_page(first);
                    self.scroll_to_page = Some(first);
                }
                self.search_results = Some(results);
                self.highlighted_query = self.search_query.clone();
                self.highlight_state = None; // force le recalcul du surlignage.
            }
            Err(e) => self.error = Some(format!("Erreur de recherche : {e}")),
        }
    }

    /// Interprète `self.goto_page_input` comme un numéro de page 1-based et
    /// y saute (`Session::goto_page` est déjà bornée : une valeur hors
    /// document est simplement ignorée). Un contenu non numérique est
    /// silencieusement ignoré plutôt que de faire planter la saisie.
    fn goto_page_from_input(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        let Ok(page_number) = self.goto_page_input.trim().parse::<usize>() else {
            return;
        };
        let Some(index) = page_number.checked_sub(1) else {
            return;
        };
        session.goto_page(index);
        self.scroll_to_page = Some(session.page_index());
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
    }

    /// Ajuste le zoom pour que la largeur de la page courante (à 100 %,
    /// indépendamment du zoom actuel) remplisse `available_width` (points
    /// logiques `egui`, la largeur de la zone de dessin de la page) — le
    /// réglage qu'utilisent la plupart des lecteurs PDF par défaut, absent
    /// jusqu'ici (zoom toujours figé à 100 % à l'ouverture).
    fn fit_to_width(&mut self, available_width: f32) {
        let Some(session) = &self.session else {
            return;
        };
        let Ok(media_box) = session.current_page_media_box() else {
            return;
        };
        let page_width = media_box[2] - media_box[0];
        if page_width <= 0.0 {
            return;
        }
        // Petite marge pour ne pas déclencher la barre de défilement
        // horizontale à cause d'un arrondi.
        self.set_zoom((available_width - 4.0) / page_width as f32);
    }

    /// "↕ Ajuster à la page" (Sprint 21, #10) : comme `fit_to_width`, mais
    /// prend le zoom le plus petit des deux dimensions pour que la page
    /// entière (largeur **et** hauteur) tienne dans la zone disponible,
    /// plutôt que seulement sa largeur.
    fn fit_to_page(&mut self, available: egui::Vec2) {
        let Some(session) = &self.session else {
            return;
        };
        let Ok(media_box) = session.current_page_media_box() else {
            return;
        };
        let page_width = media_box[2] - media_box[0];
        let page_height = media_box[3] - media_box[1];
        if page_width <= 0.0 || page_height <= 0.0 {
            return;
        }
        let zoom_for_width = (available.x - 4.0) / page_width as f32;
        let zoom_for_height = (available.y - 4.0) / page_height as f32;
        self.set_zoom(zoom_for_width.min(zoom_for_height));
    }

    /// Raccourcis clavier globaux : `⌘F` donne le focus au champ de
    /// recherche, `⌘+`/`⌘-`/`⌘0` zooment/réinitialisent, flèches
    /// gauche/droite et Page Haut/Bas changent de page — ces derniers
    /// seulement quand aucun widget texte n'a le focus (`wants_keyboard_input`)
    /// pour ne pas voler les flèches à la saisie dans le champ de recherche.
    fn handle_keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        if self.session.is_none() {
            return;
        }

        let (focus_search, focus_goto_page, zoom_in, zoom_out, zoom_reset) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::F),
                i.modifiers.command && i.key_pressed(egui::Key::G),
                i.modifiers.command
                    && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals)),
                i.modifiers.command && i.key_pressed(egui::Key::Minus),
                i.modifiers.command && i.key_pressed(egui::Key::Num0),
            )
        });
        if focus_search {
            ctx.memory_mut(|m| {
                m.request_focus(egui::Id::new(SEARCH_FIELD_ID));
            });
        }
        if focus_goto_page {
            ctx.memory_mut(|m| {
                m.request_focus(egui::Id::new(GOTO_PAGE_FIELD_ID));
            });
        }
        if zoom_in {
            self.set_zoom(self.zoom + 0.25);
        }
        if zoom_out {
            self.set_zoom(self.zoom - 0.25);
        }
        if zoom_reset {
            self.set_zoom(1.0);
        }

        if ctx.wants_keyboard_input() {
            return; // laisse les flèches au champ de texte actuellement focus.
        }
        let (prev_page, next_page) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::PageDown),
            )
        });
        if prev_page {
            if let Some(session) = &mut self.session {
                session.prev_page();
                self.scroll_to_page = Some(session.page_index());
            }
        }
        if next_page {
            if let Some(session) = &mut self.session {
                session.next_page();
                self.scroll_to_page = Some(session.page_index());
            }
        }
    }

    /// Saute à l'occurrence suivante/précédente (cyclique) dans
    /// `search_results`, sans relancer la recherche.
    fn jump_to_match(&mut self, delta: isize) {
        let Some(results) = &self.search_results else {
            return;
        };
        if results.is_empty() {
            return;
        }
        let len = results.len() as isize;
        let next = (self.search_cursor as isize + delta).rem_euclid(len) as usize;
        self.search_cursor = next;
        let target = results[next];
        if let Some(session) = &mut self.session {
            session.goto_page(target);
        }
        self.scroll_to_page = Some(target);
    }

    /// Re-rastérise la page courante si la page ou le zoom affiché a changé
    /// depuis la dernière frame ; sinon réutilise la texture GPU existante.
    fn ensure_texture(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else {
            return;
        };
        if session.page_count() == 0 {
            return;
        }

        // Quantifie le zoom pour éviter de re-rasteriser à chaque frame à
        // cause du bruit en virgule flottante des sliders.
        let zoom_key = (self.zoom * 1000.0).round() as u32;
        let page_index = session.page_index();
        // Densité de rendu (au moins `MIN_RENDER_DENSITY`, voir sa doc) :
        // sans elle, sur un écran non-Retina la page serait rastérisée à
        // 1 pixel bitmap par point, d'où un rendu flou/crénelé — en
        // particulier visible sur du texte en gras à petite taille.
        let pixel_ratio = render_density(ctx);
        let pixel_ratio_key = (pixel_ratio * 1000.0).round() as u32;
        if self.texture_state == Some((page_index, zoom_key, pixel_ratio_key)) {
            return; // déjà à jour.
        }

        match session.render_current_page(self.zoom as f64 * pixel_ratio as f64) {
            Ok(rendered) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [rendered.width as usize, rendered.height as usize],
                    &rendered.rgba,
                );
                self.texture = Some(ctx.load_texture("page", image, egui::TextureOptions::LINEAR));
                // Taille d'affichage en points logiques : la texture a été
                // rendue `pixel_ratio` fois plus grande que ça exprès (voir
                // ci-dessus), donc on divise pour revenir à la taille voulue
                // à l'écran plutôt que de laisser `egui::Image` utiliser la
                // taille de texture telle quelle.
                self.texture_logical_size = Some(egui::vec2(
                    rendered.width as f32 / pixel_ratio,
                    rendered.height as f32 / pixel_ratio,
                ));
                self.texture_state = Some((page_index, zoom_key, pixel_ratio_key));
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("Erreur de rendu page {}: {e}", page_index));
                self.texture = None;
            }
        }
    }

    /// Recalcule `highlight_rects` si la page affichée ou la requête
    /// surlignée a changé depuis le dernier appel.
    fn ensure_highlights(&mut self) {
        let Some(session) = &self.session else {
            self.highlight_rects.clear();
            return;
        };
        if self.highlighted_query.is_empty() {
            self.highlight_rects.clear();
            self.highlight_state = None;
            return;
        }

        let key = (session.page_index(), self.highlighted_query.clone());
        if self.highlight_state.as_ref() == Some(&key) {
            return; // déjà à jour.
        }

        match session.find_matches_on_current_page(&self.highlighted_query) {
            Ok(rects) => self.highlight_rects = rects,
            Err(_) => self.highlight_rects.clear(),
        }
        self.highlight_state = Some(key);
    }

    /// Charge (et met en cache) la texture de la miniature de `index`, si ce
    /// n'est pas déjà fait. Rendue à `THUMBNAIL_SCALE * pixels_per_point`
    /// pour rester nette sur écran Retina (voir `ensure_texture`) ; la
    /// taille d'affichage logique est renvoyée pour que l'appelant
    /// (`show_thumbnail_panel`) l'utilise avec `fit_to_exact_size`.
    fn ensure_thumbnail(&mut self, index: usize, ctx: &egui::Context) -> Option<egui::Vec2> {
        let pixel_ratio = render_density(ctx);
        if let Some(texture) = self.thumbnails.get(&index) {
            let [w, h] = texture.size();
            return Some(egui::vec2(w as f32 / pixel_ratio, h as f32 / pixel_ratio));
        }
        self.session.as_ref()?;
        // Pas encore en cache : demande le rendu au thread d'arrière-plan
        // (Sprint 21, #20) plutôt que de bloquer cette frame sur
        // `Session::render_page` — le résultat sera livré par
        // `drain_background_renders` à une frame suivante. `None` ici
        // signifie juste "pas encore prêt", pas une erreur.
        let scale = THUMBNAIL_SCALE * pixel_ratio as f64;
        let scale_key = (scale * 1000.0).round() as u32;
        let key = (RenderKind::Thumbnail, index, scale_key);
        if self.background_requested.insert(key) {
            self.render_worker.request_render(
                RenderKind::Thumbnail,
                index,
                scale,
                scale_key,
                self.render_generation,
            );
            ctx.request_repaint();
        }
        None
    }

    /// Dessine le panneau de miniatures et retourne l'index de page cliqué,
    /// le cas échéant (la navigation elle-même est faite par l'appelant pour
    /// éviter d'emprunter `self.session` en même temps que `self.thumbnails`).
    ///
    /// Sprint 19 (manipulation de pages) : chaque miniature est une source de
    /// glisser-déposer (`egui::Ui::dnd_drag_source`/`dnd_drop_zone`, API
    /// intégrée depuis `egui` 0.24) pour réordonner les pages, porte une case
    /// à cocher pour la sélection multiple (utilisée par "✂ Extraire…") et
    /// deux boutons 🗑/↻ (supprimer/pivoter). La barre au-dessus du panneau
    /// donne accès à l'insertion (page vierge, image, autre PDF).
    fn show_thumbnail_panel(&mut self, ctx: &egui::Context) -> Option<usize> {
        let page_count = self.session.as_ref()?.page_count();
        let current = self.session.as_ref()?.page_index();
        let mut clicked = None;
        let mut reorder: Option<(usize, usize)> = None;
        let mut delete_requested: Option<usize> = None;
        let mut rotate_requested: Option<usize> = None;

        egui::SidePanel::left("thumbnails")
            .resizable(true)
            .default_width(170.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("＋ Page")
                        .on_hover_text("Insérer une page vierge après la page courante")
                        .clicked()
                    {
                        self.insert_blank_page(current + 1);
                    }
                    if ui
                        .button("🖼 Image…")
                        .on_hover_text("Insérer une image JPEG comme nouvelle page")
                        .clicked()
                    {
                        self.insert_image_page_dialog(current + 1);
                    }
                    if ui
                        .button("📎 PDF…")
                        .on_hover_text("Fusionner un autre PDF à la fin du document")
                        .clicked()
                    {
                        self.merge_pdf_dialog();
                    }
                });
                if !self.thumbnail_selection.is_empty()
                    && ui
                        .button(format!("✂ Extraire ({})…", self.thumbnail_selection.len()))
                        .clicked()
                {
                    self.extract_selection_dialog();
                }
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for index in 0..page_count {
                        let Some(logical_size) = self.ensure_thumbnail(index, ctx) else {
                            continue;
                        };
                        let Some(texture) = self.thumbnails.get(&index).cloned() else {
                            continue;
                        };
                        let mut selected = self.thumbnail_selection.contains(&index);

                        let frame = egui::Frame::default().inner_margin(4.0);
                        let (_drop_area, dragged_from) =
                            ui.dnd_drop_zone::<usize, _>(frame, |ui| {
                                ui.dnd_drag_source(
                                    egui::Id::new("thumb_drag").with(index),
                                    index,
                                    |ui| {
                                        ui.vertical_centered(|ui| {
                                            if ui.checkbox(&mut selected, "").changed() {
                                                if selected {
                                                    self.thumbnail_selection.insert(index);
                                                } else {
                                                    self.thumbnail_selection.remove(&index);
                                                }
                                            }
                                            let image = egui::Image::new(&texture)
                                                .fit_to_exact_size(logical_size);
                                            let response = ui.add(
                                                egui::ImageButton::new(image)
                                                    .selected(index == current),
                                            );
                                            if response.clicked() {
                                                clicked = Some(index);
                                            }
                                            ui.horizontal(|ui| {
                                                ui.label(format!("{}", index + 1));
                                                if ui
                                                    .small_button("🗑")
                                                    .on_hover_text("Supprimer cette page")
                                                    .clicked()
                                                {
                                                    delete_requested = Some(index);
                                                }
                                                if ui
                                                    .small_button("↻")
                                                    .on_hover_text("Pivoter de 90°")
                                                    .clicked()
                                                {
                                                    rotate_requested = Some(index);
                                                }
                                            });
                                        });
                                    },
                                );
                            });
                        if let Some(&from) = dragged_from.as_deref() {
                            reorder = Some((from, index));
                        }
                    }
                });
            });

        if let Some((from, to)) = reorder {
            if from != to {
                self.move_page(from, to);
            }
        }
        if let Some(index) = delete_requested {
            self.delete_page(index);
        }
        if let Some(index) = rotate_requested {
            self.rotate_page(index);
        }

        clicked
    }

    /// Consomme le prochain simple clic sur la page pendant que
    /// `self.add_text_mode` est armé (#30) : ouvre `text_modal` pour saisir
    /// le texte à ajouter, positionné à l'endroit cliqué (boîte de taille
    /// fixe `NEW_TEXT_BOX_SIZE`, l'utilisateur ne la redimensionne pas
    /// encore), puis désarme le mode.
    fn handle_add_text_click(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) {
        if !response.clicked() {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        let (px, py) = screen_to_page(pos, response.rect, media_box, scale);
        let rect = [px, py - NEW_TEXT_BOX_SIZE.1, px + NEW_TEXT_BOX_SIZE.0, py];
        self.text_modal = Some(TextInputModal {
            action: PendingTextAction::AddFreeText { rect },
            input: String::new(),
        });
        self.add_text_mode = false;
    }

    /// Sélectionne (`self.selected_annotation`) l'annotation de
    /// `self.annotations` dont le rectangle contient le point cliqué, ou
    /// efface la sélection si le clic tombe en dehors de toute annotation
    /// (#32) — appelé uniquement hors du mode "📝 Ajouter texte", voir
    /// l'appelant.
    fn handle_annotation_click(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) {
        if !response.clicked() {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        let (px, py) = screen_to_page(pos, response.rect, media_box, scale);
        self.selected_annotation = self
            .annotations
            .iter()
            .find(|a| px >= a.rect[0] && px <= a.rect[2] && py >= a.rect[1] && py <= a.rect[3])
            .map(|a| a.index);
    }

    /// Démarre/poursuit/termine un glissement de déplacement ou de
    /// redimensionnement de `selected_annotation` (#32). Renvoie `true` si
    /// le glissement de cette frame concerne une annotation — l'appelant ne
    /// doit alors pas aussi le traiter comme une sélection/un glissement de
    /// sélection de texte (voir le commentaire au point d'appel).
    ///
    /// Ne modifie jamais `pdf-edit` pendant le glissement lui-même : chaque
    /// frame de `dragged()` recalcule seulement `annotation_drag_preview`
    /// (affichage), et c'est uniquement `drag_stopped()` qui commet le rect
    /// final via `Session::set_annotation_rect_on_current_page` — un seul
    /// `EditOp`/entrée d'annulation par glissement, pas une par frame.
    fn handle_annotation_drag(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) -> bool {
        if let Some(drag) = self.annotation_drag {
            if response.dragged() {
                if let Some(pos) = response.interact_pointer_pos() {
                    self.annotation_drag_preview =
                        Some(compute_annotation_drag_rect(drag, pos, scale));
                }
                return true;
            }
            if response.drag_stopped() {
                self.annotation_drag = None;
                let index = self.selected_annotation;
                let rect = self.annotation_drag_preview.take();
                if let (Some(index), Some(rect), Some(session)) = (index, rect, &mut self.session) {
                    match session.set_annotation_rect_on_current_page(index, rect) {
                        Ok(()) => self.invalidate_after_edit(),
                        Err(e) => {
                            self.error = Some(format!("Impossible de déplacer l'annotation : {e}"))
                        }
                    }
                }
                return true;
            }
            // Glissement toujours actif mais ni `dragged()` ni
            // `drag_stopped()` cette frame (rare, ex. perte de focus) :
            // consommé quand même pour ne pas laisser retomber sur la
            // sélection de texte au milieu d'un glissement.
            return true;
        }

        if !response.drag_started() {
            return false;
        }
        let Some(index) = self.selected_annotation else {
            return false;
        };
        let Some(annot) = self.annotations.iter().find(|a| a.index == index) else {
            return false;
        };
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };
        let screen_rect = page_rect_to_screen(annot.rect, response.rect, media_box, scale);
        let hit_corner = annotation_screen_corners(screen_rect)
            .into_iter()
            .find(|(_, corner_pos)| corner_pos.distance(pos) <= ANNOTATION_HANDLE_HALF * 1.8)
            .map(|(corner, _)| corner);

        if let Some(corner) = hit_corner {
            self.annotation_drag = Some(AnnotationDrag::Resize {
                corner,
                start_pointer: pos,
                start_rect: annot.rect,
            });
            self.annotation_drag_preview = Some(annot.rect);
            true
        } else if screen_rect.contains(pos) {
            self.annotation_drag = Some(AnnotationDrag::Move {
                start_pointer: pos,
                start_rect: annot.rect,
            });
            self.annotation_drag_preview = Some(annot.rect);
            true
        } else {
            false
        }
    }

    /// Consomme le prochain simple clic sur la page tombant dans le rect
    /// d'un champ de formulaire texte de `self.form_fields` (Sprint 23) :
    /// ouvre `text_modal` préremplie avec la valeur actuelle du champ.
    /// Renvoie `true` si le clic a été consommé (appelant : ne pas aussi
    /// traiter ce même clic comme une sélection/un clic d'annotation).
    fn handle_form_field_click(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) -> bool {
        if !response.clicked() {
            return false;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };
        let (px, py) = screen_to_page(pos, response.rect, media_box, scale);
        let Some(field) = self
            .form_fields
            .iter()
            .find(|f| px >= f.rect[0] && px <= f.rect[2] && py >= f.rect[1] && py <= f.rect[3])
        else {
            return false;
        };
        self.text_modal = Some(TextInputModal {
            action: PendingTextAction::FillForm {
                field_name: field.name.clone(),
            },
            input: field.value.clone(),
        });
        true
    }

    /// Affiche la boîte de dialogue de saisie de texte (#30/#40), le cas
    /// échéant — une fenêtre flottante `egui::Window` plutôt qu'un panneau
    /// fixe, pour rester au-dessus du contenu de la page sans en changer la
    /// disposition.
    fn show_text_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = &mut self.text_modal else {
            return;
        };
        let title = match modal.action {
            PendingTextAction::AddFreeText { .. } => "Ajouter du texte",
            PendingTextAction::ReplaceText { .. } => "Remplacer le texte sélectionné",
            PendingTextAction::FillForm { .. } => "Remplir le champ de formulaire",
        };
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut modal.input)
                        .id(egui::Id::new("text_modal_input")),
                );
                response.request_focus();
                let submitted =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() || submitted {
                        confirmed = true;
                    }
                    if ui.button("Annuler").clicked() {
                        cancelled = true;
                    }
                });
            });
        if confirmed {
            self.confirm_text_modal();
        } else if cancelled {
            self.text_modal = None;
        }
    }

    /// Met à jour la sélection de texte à partir du glissement de souris sur
    /// `response` (l'image de la page), et recalcule le texte/les
    /// rectangles sélectionnés en conséquence. Invalide la sélection
    /// précédente si la page affichée a changé entre-temps.
    fn handle_text_selection(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) {
        let Some(page_index) = self.session.as_ref().map(|s| s.page_index()) else {
            return;
        };
        if self.selection_page != Some(page_index) {
            self.selection_anchor = None;
            self.selection_cursor = None;
            self.selection_rects.clear();
            self.selection_text.clear();
            self.selection_page = Some(page_index);
        }

        // Triple-clic (ligne) et double-clic (mot) avant le glissement : un
        // double/triple-clic est aussi rapporté comme `clicked()`/parfois
        // `drag_started()` par `egui` selon la plateforme, donc l'ordre des
        // branches ci-dessous importe — les cas les plus spécifiques
        // d'abord.
        let word_or_line_click = if response.triple_clicked() {
            Some(false) // false = ligne
        } else if response.double_clicked() {
            Some(true) // true = mot
        } else {
            None
        };

        if let Some(is_word) = word_or_line_click {
            if let Some(pos) = response.interact_pointer_pos() {
                let point = screen_to_page(pos, response.rect, media_box, scale);
                let index = self
                    .session
                    .as_ref()
                    .and_then(|s| s.char_index_at_on_current_page(point).ok().flatten());
                if let Some(index) = index {
                    let range = self.session.as_ref().and_then(|s| {
                        if is_word {
                            s.word_range_at_on_current_page(index).ok()
                        } else {
                            s.line_range_at_on_current_page(index).ok()
                        }
                    });
                    if let Some(range) = range.filter(|r| r.end > r.start) {
                        self.selection_anchor = Some(range.start);
                        self.selection_cursor = Some(range.end - 1);
                    }
                }
            }
        } else if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let point = screen_to_page(pos, response.rect, media_box, scale);
                self.selection_anchor = self
                    .session
                    .as_ref()
                    .and_then(|s| s.char_index_at_on_current_page(point).ok().flatten());
                self.selection_cursor = self.selection_anchor;
            }
        } else if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let point = screen_to_page(pos, response.rect, media_box, scale);
                self.selection_cursor = self
                    .session
                    .as_ref()
                    .and_then(|s| s.char_index_at_on_current_page(point).ok().flatten());
            }
        } else if response.clicked() {
            // Simple clic (pas de glissement) : efface la sélection.
            self.selection_anchor = None;
            self.selection_cursor = None;
        }

        // Pas de garde `a != b` : un double/triple-clic sur un mot/une ligne
        // d'un seul caractère doit tout de même produire une sélection d'un
        // caractère (voir ci-dessus) ; un simple clic sans glissement est
        // géré séparément (`response.clicked()`, efface déjà la sélection).
        let range = match (self.selection_anchor, self.selection_cursor) {
            (Some(a), Some(b)) => Some(a.min(b)..a.max(b) + 1),
            _ => None,
        };
        match range {
            Some(range) => match self
                .session
                .as_ref()
                .map(|s| s.selection_on_current_page(range))
            {
                Some(Ok((text, rects))) => {
                    self.selection_text = text;
                    self.selection_rects = rects;
                }
                _ => {
                    self.selection_text.clear();
                    self.selection_rects.clear();
                }
            },
            None => {
                self.selection_text.clear();
                self.selection_rects.clear();
            }
        }
    }

    /// Dessine le panneau de signets (table des matières) et retourne
    /// l'index de page cliqué, le cas échéant. `None` (pas seulement un
    /// panneau vide) si le document n'a pas de `/Outlines` — l'appelant
    /// n'affiche alors pas le panneau du tout.
    fn show_outline_panel(&mut self, ctx: &egui::Context) -> Option<usize> {
        let outline = self.session.as_ref()?.outline().ok()?;
        if outline.is_empty() {
            return None;
        }

        let mut clicked = None;
        egui::SidePanel::left("outline")
            .resizable(true)
            .default_width(180.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    render_outline_items(ui, &outline, 0, &mut clicked);
                });
            });
        clicked
    }

    /// Affiche toutes les pages du document empilées verticalement dans une
    /// seule zone de défilement. Virtualisé via `ScrollArea::show_rows` :
    /// seules les pages dont la ligne tombe dans (ou près de) la zone
    /// visible sont rastérisées/chargées en texture, ce qui reste praticable
    /// sur un document de plusieurs centaines de pages.
    fn show_continuous_scroll(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Densité de rendu (voir `ensure_texture`/`render_density` pour le
        // même souci en mode page unique) : sans elle, les pages du
        // défilement continu seraient floues/crénelées sur un écran
        // non-Retina.
        let pixel_ratio = render_density(ctx);
        let zoom_key = (self.zoom * 1000.0).round() as u32;
        let pixel_ratio_key = (pixel_ratio * 1000.0).round() as u32;
        if self.page_textures_zoom_key != Some((zoom_key, pixel_ratio_key)) {
            self.page_textures.clear();
            self.page_textures_zoom_key = Some((zoom_key, pixel_ratio_key));
        }

        let zoom = self.zoom as f64;
        let Some(session) = &self.session else {
            return;
        };
        let page_count = session.page_count();
        if page_count == 0 {
            return;
        }
        // Hauteur de ligne uniforme dérivée de la page 0 (voir limitation
        // en tête de fichier : documents à pages de tailles hétérogènes).
        let Ok(media_box) = session.page_media_box(0) else {
            return;
        };
        let page_logical_size = egui::vec2(
            ((media_box[2] - media_box[0]) * zoom) as f32,
            ((media_box[3] - media_box[1]) * zoom) as f32,
        );
        let row_height = page_logical_size.y + 8.0;

        let mut scroll_area = egui::ScrollArea::vertical();
        if let Some(target) = self.scroll_to_page.take() {
            scroll_area = scroll_area.vertical_scroll_offset(target as f32 * row_height);
        }

        // Sprint 21 (#20) : les pages manquantes sont demandées au thread de
        // rendu en arrière-plan plutôt que rastérisées ici de façon
        // bloquante — c'est justement ce qui pouvait geler l'UI sur un gros
        // document en défilement continu. Le résultat, s'il n'est pas
        // encore prêt, laisse simplement un espace réservé (`add_space`)
        // pour ne pas faire sauter le défilement une fois la page arrivée.
        let page_textures = &mut self.page_textures;
        let background_requested = &mut self.background_requested;
        let render_worker = &self.render_worker;
        let render_generation = self.render_generation;
        scroll_area.show_rows(ui, row_height, page_count, |ui, row_range| {
            for index in row_range {
                if !page_textures.contains_key(&index) {
                    let scale = zoom * pixel_ratio as f64;
                    let scale_key = (scale * 1000.0).round() as u32;
                    let key = (RenderKind::ContinuousPage, index, scale_key);
                    if background_requested.insert(key) {
                        render_worker.request_render(
                            RenderKind::ContinuousPage,
                            index,
                            scale,
                            scale_key,
                            render_generation,
                        );
                        ctx.request_repaint();
                    }
                }
                if let Some(texture) = page_textures.get(&index) {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new(texture).fit_to_exact_size(page_logical_size));
                    });
                } else {
                    ui.add_space(page_logical_size.y);
                }
                ui.add_space(8.0);
            }
        });
    }

    /// Cœur du rendu de cet onglet pour la frame courante — tout ce que
    /// faisait `ViewerApp::update` avant l'introduction des onglets
    /// multi-documents (Sprint 49), à l'exception de la barre de menus
    /// native et de la barre d'onglets elle-même (globales à l'application,
    /// gérées par `ViewerApp::update`). Retourne les actions qui doivent
    /// remonter au niveau de l'application (voir `DocumentTabOutcome`).
    fn update_content(&mut self, ctx: &egui::Context) -> DocumentTabOutcome {
        let mut outcome = DocumentTabOutcome::default();

        // Récupère les miniatures/pages rastérisées en arrière-plan depuis
        // la frame précédente (Sprint 21, #20) — non bloquant, avant tout
        // dessin qui pourrait en avoir besoin cette frame.
        self.drain_background_renders(ctx);

        self.handle_keyboard_shortcuts(ctx);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            // Défilement horizontal plutôt que retour à la ligne
            // (`horizontal_wrapped`) : avec beaucoup de boutons (page, zoom,
            // sélection, undo/redo...), le retour à la ligne réorganisait le
            // groupement visuel de façon imprévisible selon la largeur
            // (boutons isolés, grands espaces vides). Un simple défilement
            // horizontal garde l'ordre et le regroupement stables — on
            // molette/glisse pour voir la suite si la fenêtre est étroite,
            // plutôt que de voir la barre se réarranger.
            egui::ScrollArea::horizontal()
                .id_salt("toolbar_row_main")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // "Ouvrir…" ouvre toujours un **nouvel onglet**
                        // (Sprint 49) plutôt que de remplacer le document de
                        // cet onglet — voir `DocumentTabOutcome`.
                        if ui.button("Ouvrir…").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("PDF", &["pdf"])
                                .pick_file()
                            {
                                outcome.open_in_new_tab = Some(path);
                            }
                        }
                        ui.add_enabled_ui(self.session.is_some(), |ui| {
                            if ui.button("📄 Exporter le texte…").clicked() {
                                self.export_text_dialog();
                            }
                        });

                        ui.separator();

                        let (page_index, page_count) = self
                            .session
                            .as_ref()
                            .map(|s| (s.page_index(), s.page_count()))
                            .unwrap_or((0, 0));
                        let has_doc = self.session.is_some();

                        ui.add_enabled_ui(has_doc, |ui| {
                            ui.toggle_value(&mut self.show_thumbnails, "🖼 Miniatures");
                            ui.toggle_value(&mut self.show_outline, "📑 Signets");
                            ui.toggle_value(&mut self.continuous_scroll, "📜 Continu");
                        });

                        ui.separator();

                        ui.add_enabled_ui(has_doc && page_index > 0, |ui| {
                            if ui.button("◀ Précédente").clicked() {
                                if let Some(session) = &mut self.session {
                                    session.prev_page();
                                    self.scroll_to_page = Some(session.page_index());
                                }
                            }
                        });
                        if has_doc {
                            ui.label("Page");
                            let response = ui.add_sized(
                                [40.0, ui.available_height().min(20.0)],
                                egui::TextEdit::singleline(&mut self.goto_page_input)
                                    .id(egui::Id::new(GOTO_PAGE_FIELD_ID))
                                    .hint_text((page_index + 1).to_string()),
                            );
                            let submitted = response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if submitted {
                                self.goto_page_from_input();
                                self.goto_page_input.clear();
                            }
                            ui.label(format!("/ {}", page_count.max(1)));
                        } else {
                            ui.label("Aucun document");
                        }
                        ui.add_enabled_ui(has_doc && page_index + 1 < page_count, |ui| {
                            if ui.button("Suivante ▶").clicked() {
                                if let Some(session) = &mut self.session {
                                    session.next_page();
                                    self.scroll_to_page = Some(session.page_index());
                                }
                            }
                        });

                        ui.separator();

                        ui.add_enabled_ui(has_doc, |ui| {
                            if ui.button("－").clicked() {
                                self.set_zoom(self.zoom - 0.25);
                            }
                            ui.label(format!("{:.0}%", self.zoom * 100.0));
                            if ui.button("＋").clicked() {
                                self.set_zoom(self.zoom + 0.25);
                            }
                            if ui
                                .button("Réinitialiser")
                                .on_hover_text("Taille réelle (100 %)")
                                .clicked()
                            {
                                self.set_zoom(1.0);
                            }
                            if ui.button("↔ Ajuster à la largeur").clicked() {
                                self.fit_width_requested = true;
                            }
                            if ui.button("↕ Ajuster à la page").clicked() {
                                self.fit_page_requested = true;
                            }
                        });

                        if !self.selection_text.is_empty() {
                            ui.separator();
                            if ui.button("📋 Copier").clicked() {
                                ui.output_mut(|o| o.copied_text = self.selection_text.clone());
                            }
                            if ui.button("🖍 Surligner").clicked() {
                                self.highlight_selection();
                            }
                            if ui.button("Souligner").clicked() {
                                self.underline_selection();
                            }
                            if ui.button("Barrer").clicked() {
                                self.strikeout_selection();
                            }
                            if ui.button("✏ Remplacer…").clicked() {
                                self.open_replace_text_modal();
                            }
                        }

                        ui.add_enabled_ui(has_doc, |ui| {
                            ui.separator();
                            ui.toggle_value(&mut self.add_text_mode, "📝 Ajouter texte")
                                .on_hover_text("Cliquer sur la page pour placer un nouveau texte");
                            if self.selected_annotation.is_some()
                                && ui.button("🗑 Supprimer l'annotation").clicked()
                            {
                                self.delete_selected_annotation();
                            }
                            // #32 (Sprint 55) : le réglage couleur/opacité
                            // ne s'applique qu'aux annotations dont
                            // `set_annotation_style` sait régénérer
                            // l'apparence (voir sa doc) — pas `/FreeText`,
                            // dont l'apparence encode le texte lui-même.
                            if let Some(selected) = self.selected_annotation {
                                let styleable = self
                                    .annotations
                                    .iter()
                                    .find(|a| a.index == selected)
                                    .is_some_and(|a| {
                                        matches!(
                                            a.subtype.as_str(),
                                            "Highlight" | "Underline" | "StrikeOut"
                                        )
                                    });
                                if styleable && ui.button("🎨 Style…").clicked() {
                                    self.open_annotation_style_popup(selected);
                                }
                            }
                        });

                        if let Some(session) = &self.session {
                            ui.separator();
                            let (can_undo, can_redo) =
                                (session.can_undo_edit(), session.can_redo_edit());
                            ui.add_enabled_ui(can_undo, |ui| {
                                if ui.button("↶ Annuler").clicked() {
                                    self.undo_edit();
                                }
                            });
                            ui.add_enabled_ui(can_redo, |ui| {
                                if ui.button("↷ Rétablir").clicked() {
                                    self.redo_edit();
                                }
                            });
                            if ui.button("💾 Enregistrer").clicked() {
                                self.save_in_place();
                            }
                            if ui.button("🖨 Imprimer…").clicked() {
                                self.print_document();
                            }
                            if ui.button("🗜 Optimiser…").clicked() {
                                self.export_optimized_dialog();
                            }
                        }

                        if let Some(session) = &self.session {
                            ui.separator();
                            ui.label(
                                session
                                    .path()
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy(),
                            );
                        }
                    });
                });

            egui::ScrollArea::horizontal()
                .id_salt("toolbar_row_search")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let has_doc = self.session.is_some();
                        ui.add_enabled_ui(has_doc, |ui| {
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .id(egui::Id::new(SEARCH_FIELD_ID)),
                            );
                            let submitted = response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.button("🔍 Rechercher").clicked() || submitted {
                                self.run_search();
                            }
                        });

                        if let Some(results) = &self.search_results {
                            if results.is_empty() {
                                ui.label("Aucun résultat");
                            } else {
                                ui.label(format!(
                                    "{}/{} pages",
                                    self.search_cursor + 1,
                                    results.len()
                                ));
                                if ui.button("◀ Précédent").clicked() {
                                    self.jump_to_match(-1);
                                }
                                if ui.button("Suivant ▶").clicked() {
                                    self.jump_to_match(1);
                                }
                            }
                        }
                    });
                });
        });

        if self.show_thumbnails {
            if let Some(clicked) = self.show_thumbnail_panel(ctx) {
                if let Some(session) = &mut self.session {
                    session.goto_page(clicked);
                }
                self.scroll_to_page = Some(clicked);
            }
        }

        if self.show_outline {
            if let Some(clicked) = self.show_outline_panel(ctx) {
                if let Some(session) = &mut self.session {
                    session.goto_page(clicked);
                }
                self.scroll_to_page = Some(clicked);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
                return;
            }
            if self.session.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Ouvrez un fichier PDF pour commencer.");
                });
                return;
            }

            // Molette+Ctrl ou pincement trackpad : `zoom_delta` vaut 1.0 en
            // l'absence de tel geste (simple scroll de la molette, laissé au
            // `ScrollArea` ci-dessous pour le défilement). Sprint 21 (#9) :
            // recentre le défilement autour du curseur plutôt que de laisser
            // le zoom dériver vers le coin haut-gauche — calcule le nouveau
            // décalage de défilement à partir du dernier connu
            // (`last_scroll_offset`/`last_scroll_viewport`, mis à jour à
            // chaque frame par la `ScrollArea` plus bas) et le fait consommer
            // par cette même `ScrollArea` au prochain rendu.
            let zoom_delta = ctx.input(|i| i.zoom_delta());
            if zoom_delta != 1.0 && !self.continuous_scroll {
                let old_zoom = self.zoom;
                self.set_zoom(self.zoom * zoom_delta);
                let new_zoom = self.zoom;
                if let (Some(viewport), Some(hover)) = (
                    self.last_scroll_viewport,
                    ctx.input(|i| i.pointer.hover_pos()),
                ) {
                    if viewport.contains(hover) && old_zoom > 0.0 {
                        let cursor_in_viewport = hover - viewport.min;
                        let content_point_old = self.last_scroll_offset + cursor_in_viewport;
                        let ratio = new_zoom / old_zoom;
                        let new_offset = content_point_old * ratio - cursor_in_viewport;
                        self.pending_scroll_offset = Some(new_offset);
                    }
                }
            } else if zoom_delta != 1.0 {
                self.set_zoom(self.zoom * zoom_delta);
            }

            // ⌘C/Ctrl+C : copie la sélection de texte courante, si non vide
            // (egui traduit déjà le raccourci plateforme en `Event::Copy`).
            if !self.selection_text.is_empty()
                && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
            {
                let text = self.selection_text.clone();
                ctx.output_mut(|o| o.copied_text = text);
            }

            if self.continuous_scroll {
                self.show_continuous_scroll(ui, ctx);
                return;
            }

            if self.fit_width_requested {
                self.fit_width_requested = false;
                self.fit_to_width(ui.available_width());
            }
            if self.fit_page_requested {
                self.fit_page_requested = false;
                self.fit_to_page(ui.available_size());
            }

            self.ensure_texture(ctx);
            self.ensure_highlights();
            self.ensure_annotations();
            self.ensure_form_fields();
            self.ensure_checkbox_fields();
            self.ensure_radio_groups();
            self.ensure_choice_fields();

            // Clonée (bon marché : `TextureHandle` est un handle partagé)
            // pour ne pas garder de prêt sur `self.texture` pendant qu'on
            // met à jour `self.selection_*` plus bas dans la même closure.
            if let Some(texture) = self.texture.clone() {
                let media_box = self
                    .session
                    .as_ref()
                    .and_then(|s| s.current_page_media_box().ok());
                let scale = self.zoom as f64;

                let mut scroll_area = egui::ScrollArea::both();
                if let Some(offset) = self.pending_scroll_offset.take() {
                    scroll_area =
                        scroll_area.scroll_offset(egui::vec2(offset.x.max(0.0), offset.y.max(0.0)));
                }
                let scroll_output = scroll_area.show(ui, |ui| {
                    // `fit_to_exact_size` : la texture est rendue à
                    // `zoom * pixels_per_point` pixels (voir `ensure_texture`)
                    // pour rester nette sur écran Retina, donc affichée plus
                    // grande que sa taille logique voulue si on laissait
                    // `egui` déduire la taille d'affichage de la texture.
                    let mut image = egui::Image::new(&texture).sense(egui::Sense::click_and_drag());
                    if let Some(size) = self.texture_logical_size {
                        image = image.fit_to_exact_size(size);
                    }
                    let response = ui.add(image);

                    if let Some(media_box) = media_box {
                        if !self.highlight_rects.is_empty() {
                            draw_highlights(
                                ui,
                                &response,
                                &self.highlight_rects,
                                media_box,
                                scale,
                                HIGHLIGHT_COLOR,
                            );
                        }

                        if !self.annotations.is_empty() {
                            draw_annotation_outlines(
                                ui,
                                &response,
                                &self.annotations,
                                media_box,
                                scale,
                                self.selected_annotation,
                                self.annotation_drag_preview,
                            );
                        }

                        if !self.form_fields.is_empty() {
                            draw_form_field_outlines(
                                ui,
                                &response,
                                &self.form_fields,
                                media_box,
                                scale,
                            );
                        }

                        if !self.checkbox_fields.is_empty() {
                            draw_checkbox_field_outlines(
                                ui,
                                &response,
                                &self.checkbox_fields,
                                media_box,
                                scale,
                            );
                        }

                        if !self.radio_groups.is_empty() {
                            draw_radio_group_outlines(
                                ui,
                                &response,
                                &self.radio_groups,
                                media_box,
                                scale,
                            );
                        }

                        if !self.choice_fields.is_empty() {
                            draw_choice_field_outlines(
                                ui,
                                &response,
                                &self.choice_fields,
                                media_box,
                                scale,
                            );
                        }

                        // Le mode "📝 Ajouter texte" (#30) intercepte le
                        // simple clic à la place de la sélection/l'annotation
                        // (voir la doc de `handle_add_text_click`) — les deux
                        // usages ne doivent jamais se marcher dessus sur le
                        // même clic. Un clic tombant dans un champ de
                        // formulaire (#Sprint 23), une case à cocher (Sprint
                        // 52), un bouton radio (Sprint 53) ou un champ
                        // liste/menu déroulant (Sprint 54) a priorité sur la
                        // sélection/l'annotation, pour la même raison.
                        if self.add_text_mode {
                            self.handle_add_text_click(&response, media_box, scale);
                        } else if !self.handle_checkbox_field_click(&response, media_box, scale)
                            && !self.handle_radio_group_click(&response, media_box, scale)
                            && !self.handle_choice_field_click(&response, media_box, scale)
                            && !self.handle_form_field_click(&response, media_box, scale)
                        {
                            // Un glissement sur la sélection courante (#32,
                            // déplacement ou poignée de redimensionnement) a
                            // priorité sur la sélection/la sélection de texte
                            // pour le même geste — sinon le même glissement
                            // finirait aussi interprété comme une sélection
                            // de texte sous l'annotation.
                            if !self.handle_annotation_drag(&response, media_box, scale) {
                                self.handle_annotation_click(&response, media_box, scale);
                                self.handle_text_selection(&response, media_box, scale);
                            }
                        }
                        if !self.selection_rects.is_empty() {
                            draw_highlights(
                                ui,
                                &response,
                                &self.selection_rects,
                                media_box,
                                scale,
                                SELECTION_COLOR,
                            );
                        }
                    }
                });
                self.last_scroll_offset = scroll_output.state.offset;
                self.last_scroll_viewport = Some(scroll_output.inner_rect);
            }
        });

        self.show_text_modal(ctx);
        self.show_choice_field_popup(ctx);
        self.show_annotation_style_popup(ctx);

        outcome
    }
}

/// Affiche récursivement une table des matières, avec une indentation par
/// niveau de profondeur. Les entrées sans page résolue (voir
/// `pdf_app::OutlineItem::page_index`, ex. destination nommée non gérée)
/// restent affichées mais ne naviguent nulle part au clic.
fn render_outline_items(
    ui: &mut egui::Ui,
    items: &[pdf_app::OutlineItem],
    depth: usize,
    clicked: &mut Option<usize>,
) {
    for item in items {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 12.0);
            if ui.selectable_label(false, &item.title).clicked() {
                if let Some(page) = item.page_index {
                    *clicked = Some(page);
                }
            }
        });
        render_outline_items(ui, &item.children, depth + 1, clicked);
    }
}

/// Dessine les rectangles de surlignage (espace page PDF, origine
/// bas-gauche) par-dessus l'image de la page déjà affichée à `image_response`,
/// en reproduisant la même transformation page->pixmap que `pdf-render`
/// (voir `pdf_render::page_flip_matrix`) : c'est le prix de garder `pdf-ui`
/// indépendant du backend de rendu — cette conversion devra suivre si le
/// calcul de transformation change côté rendu.
fn draw_highlights(
    ui: &egui::Ui,
    image_response: &egui::Response,
    rects: &[pdf_app::MatchRect],
    media_box: [f64; 4],
    scale: f64,
    color: egui::Color32,
) {
    let origin_x = media_box[0];
    let origin_top = media_box[3];
    let painter = ui.painter();
    for rect in rects {
        let min = egui::pos2(
            image_response.rect.min.x + ((rect.x0 - origin_x) * scale) as f32,
            image_response.rect.min.y + ((origin_top - rect.y1) * scale) as f32,
        );
        let max = egui::pos2(
            image_response.rect.min.x + ((rect.x1 - origin_x) * scale) as f32,
            image_response.rect.min.y + ((origin_top - rect.y0) * scale) as f32,
        );
        painter.rect_filled(egui::Rect::from_min_max(min, max), 0.0, color);
    }
}

/// Dessine un contour (pas un remplissage, contrairement à `draw_highlights`)
/// par annotation de `annotations` (Sprint 20, #32) — l'annotation
/// `selected` (le cas échéant) a un trait plus épais pour rester
/// identifiable avant de cliquer "🗑 Supprimer l'annotation".
/// Convertit un rectangle espace page (`[x0 y0 x1 y1]`, origine bas-gauche)
/// en rectangle écran dans le repère de `image_rect` — même transformation
/// que `draw_highlights`/`screen_to_page`, factorisée ici pour être partagée
/// par le dessin du contour, celui des poignées et leur détection de clic
/// (#32).
fn page_rect_to_screen(
    rect: [f64; 4],
    image_rect: egui::Rect,
    media_box: [f64; 4],
    scale: f64,
) -> egui::Rect {
    let origin_x = media_box[0];
    let origin_top = media_box[3];
    let min = egui::pos2(
        image_rect.min.x + ((rect[0] - origin_x) * scale) as f32,
        image_rect.min.y + ((origin_top - rect[3]) * scale) as f32,
    );
    let max = egui::pos2(
        image_rect.min.x + ((rect[2] - origin_x) * scale) as f32,
        image_rect.min.y + ((origin_top - rect[1]) * scale) as f32,
    );
    egui::Rect::from_min_max(min, max)
}

/// Les 4 coins de `rect` associés au `Corner` qu'ils représentent, dans
/// l'ordre où `page_rect_to_screen` les range (`min`/`max` XY, pas
/// nécessairement haut-gauche/bas-droit en espace écran vs page — la page a
/// l'origine en bas, l'écran en haut, donc `rect.min` en écran correspond au
/// coin **haut-gauche** en page).
fn annotation_screen_corners(screen_rect: egui::Rect) -> [(Corner, egui::Pos2); 4] {
    [
        (Corner::TopLeft, screen_rect.min),
        (
            Corner::TopRight,
            egui::pos2(screen_rect.max.x, screen_rect.min.y),
        ),
        (
            Corner::BottomLeft,
            egui::pos2(screen_rect.min.x, screen_rect.max.y),
        ),
        (Corner::BottomRight, screen_rect.max),
    ]
}

/// Calcule le rect espace page d'une annotation en cours de glissement à la
/// position pointeur écran `pos` (#32) — voir `handle_annotation_drag`.
/// Recalculé depuis `start_pointer`/`start_rect` (pas accumulé delta par
/// delta d'une frame à l'autre) pour éviter toute dérive numérique sur un
/// glissement long ; `y` écran croît vers le bas, `y` page vers le haut,
/// d'où le signe opposé entre `dx`/`dy`.
fn compute_annotation_drag_rect(drag: AnnotationDrag, pos: egui::Pos2, scale: f64) -> [f64; 4] {
    match drag {
        AnnotationDrag::Move {
            start_pointer,
            start_rect,
        } => {
            let dx = (pos.x - start_pointer.x) as f64 / scale;
            let dy = (pos.y - start_pointer.y) as f64 / scale;
            [
                start_rect[0] + dx,
                start_rect[1] - dy,
                start_rect[2] + dx,
                start_rect[3] - dy,
            ]
        }
        AnnotationDrag::Resize {
            corner,
            start_pointer,
            start_rect,
        } => {
            let dx = (pos.x - start_pointer.x) as f64 / scale;
            let dy = (pos.y - start_pointer.y) as f64 / scale;
            let [x0, y0, x1, y1] = start_rect;
            let (mut nx0, mut ny0, mut nx1, mut ny1) = (x0, y0, x1, y1);
            match corner {
                Corner::TopLeft => {
                    nx0 = x0 + dx;
                    ny1 = y1 - dy;
                }
                Corner::TopRight => {
                    nx1 = x1 + dx;
                    ny1 = y1 - dy;
                }
                Corner::BottomLeft => {
                    nx0 = x0 + dx;
                    ny0 = y0 - dy;
                }
                Corner::BottomRight => {
                    nx1 = x1 + dx;
                    ny0 = y0 - dy;
                }
            }
            // Empêche un rect dégénéré/inversé si la poignée est glissée
            // au-delà du coin opposé : referme à `ANNOTATION_MIN_SIZE` du
            // côté fixe plutôt que de laisser `x0 > x1`/`y0 > y1`.
            match corner {
                Corner::TopLeft | Corner::BottomLeft => nx0 = nx0.min(x1 - ANNOTATION_MIN_SIZE),
                Corner::TopRight | Corner::BottomRight => nx1 = nx1.max(x0 + ANNOTATION_MIN_SIZE),
            }
            match corner {
                Corner::TopLeft | Corner::TopRight => ny1 = ny1.max(y0 + ANNOTATION_MIN_SIZE),
                Corner::BottomLeft | Corner::BottomRight => ny0 = ny0.min(y1 - ANNOTATION_MIN_SIZE),
            }
            [nx0, ny0, nx1, ny1]
        }
    }
}

fn draw_annotation_outlines(
    ui: &egui::Ui,
    image_response: &egui::Response,
    annotations: &[pdf_app::AnnotationInfo],
    media_box: [f64; 4],
    scale: f64,
    selected: Option<usize>,
    drag_preview: Option<[f64; 4]>,
) {
    let painter = ui.painter();
    for annot in annotations {
        let is_selected = selected == Some(annot.index);
        let rect = if is_selected {
            drag_preview.unwrap_or(annot.rect)
        } else {
            annot.rect
        };
        let screen_rect = page_rect_to_screen(rect, image_response.rect, media_box, scale);
        let width = if is_selected { 2.5 } else { 1.0 };
        painter.rect_stroke(
            screen_rect,
            0.0,
            egui::Stroke::new(width, ANNOTATION_OUTLINE_COLOR),
        );
        if is_selected {
            for (_, corner_pos) in annotation_screen_corners(screen_rect) {
                painter.rect_filled(
                    egui::Rect::from_center_size(
                        corner_pos,
                        egui::vec2(ANNOTATION_HANDLE_HALF * 2.0, ANNOTATION_HANDLE_HALF * 2.0),
                    ),
                    1.0,
                    ANNOTATION_OUTLINE_COLOR,
                );
            }
        }
    }
}

/// Dessine un contour (comme `draw_annotation_outlines`) par champ de
/// formulaire texte de `fields` (Sprint 23) — indique où cliquer pour
/// ouvrir la modale de saisie, sans distinction de sélection (contrairement
/// aux annotations, un champ ne se "sélectionne" pas avant suppression).
fn draw_form_field_outlines(
    ui: &egui::Ui,
    image_response: &egui::Response,
    fields: &[pdf_edit::FormFieldInfo],
    media_box: [f64; 4],
    scale: f64,
) {
    let origin_x = media_box[0];
    let origin_top = media_box[3];
    let painter = ui.painter();
    for field in fields {
        let rect = field.rect;
        let min = egui::pos2(
            image_response.rect.min.x + ((rect[0] - origin_x) * scale) as f32,
            image_response.rect.min.y + ((origin_top - rect[3]) * scale) as f32,
        );
        let max = egui::pos2(
            image_response.rect.min.x + ((rect[2] - origin_x) * scale) as f32,
            image_response.rect.min.y + ((origin_top - rect[1]) * scale) as f32,
        );
        painter.rect_stroke(
            egui::Rect::from_min_max(min, max),
            0.0,
            egui::Stroke::new(1.0, FORM_FIELD_OUTLINE_COLOR),
        );
    }
}

/// Dessine un contour par case à cocher de `fields` (Sprint 52, #43 suite) —
/// comme `draw_form_field_outlines`, mais coché est en plus indiqué par un
/// remplissage translucide (pas de modale à ouvrir pour voir l'état, un clic
/// bascule directement — voir `handle_checkbox_field_click`).
fn draw_checkbox_field_outlines(
    ui: &egui::Ui,
    image_response: &egui::Response,
    fields: &[pdf_edit::CheckboxFieldInfo],
    media_box: [f64; 4],
    scale: f64,
) {
    let painter = ui.painter();
    for field in fields {
        let screen_rect = page_rect_to_screen(field.rect, image_response.rect, media_box, scale);
        if field.checked {
            painter.rect_filled(
                screen_rect,
                1.0,
                CHECKBOX_FIELD_OUTLINE_COLOR.gamma_multiply(0.35),
            );
        }
        painter.rect_stroke(
            screen_rect,
            0.0,
            egui::Stroke::new(1.0, CHECKBOX_FIELD_OUTLINE_COLOR),
        );
    }
}

/// Dessine un contour par option de chaque groupe de `groups` (Sprint 53,
/// #43 suite) — même traitement visuel que `draw_checkbox_field_outlines`
/// (remplissage translucide si sélectionnée), une option à la fois plutôt
/// qu'un seul champ.
fn draw_radio_group_outlines(
    ui: &egui::Ui,
    image_response: &egui::Response,
    groups: &[pdf_edit::RadioGroupInfo],
    media_box: [f64; 4],
    scale: f64,
) {
    let painter = ui.painter();
    for group in groups {
        for option in &group.options {
            let screen_rect =
                page_rect_to_screen(option.rect, image_response.rect, media_box, scale);
            if option.selected {
                painter.rect_filled(
                    screen_rect,
                    1.0,
                    CHECKBOX_FIELD_OUTLINE_COLOR.gamma_multiply(0.35),
                );
            }
            painter.rect_stroke(
                screen_rect,
                0.0,
                egui::Stroke::new(1.0, CHECKBOX_FIELD_OUTLINE_COLOR),
            );
        }
    }
}

/// Dessine un contour par champ liste/menu déroulant de `fields` (Sprint 54,
/// #43 suite) — comme `draw_form_field_outlines`, une seule zone cliquable
/// par champ (pas une par option, contrairement à `draw_radio_group_outlines`)
/// puisqu'un clic ouvre une fenêtre de sélection plutôt que de basculer
/// directement un état.
fn draw_choice_field_outlines(
    ui: &egui::Ui,
    image_response: &egui::Response,
    fields: &[pdf_edit::ChoiceFieldInfo],
    media_box: [f64; 4],
    scale: f64,
) {
    let painter = ui.painter();
    for field in fields {
        let screen_rect = page_rect_to_screen(field.rect, image_response.rect, media_box, scale);
        painter.rect_stroke(
            screen_rect,
            0.0,
            egui::Stroke::new(1.0, FORM_FIELD_OUTLINE_COLOR),
        );
    }
}

/// Convertit une position écran (dans le repère de `image_rect`, le
/// rectangle occupé par l'image de la page) en point d'espace page PDF —
/// inverse de la transformation appliquée par `draw_highlights`.
fn screen_to_page(
    pos: egui::Pos2,
    image_rect: egui::Rect,
    media_box: [f64; 4],
    scale: f64,
) -> (f64, f64) {
    let origin_x = media_box[0];
    let origin_top = media_box[3];
    let page_x = origin_x + (pos.x - image_rect.min.x) as f64 / scale;
    let page_y = origin_top - (pos.y - image_rect.min.y) as f64 / scale;
    (page_x, page_y)
}

/// Application complète — Sprint 49 : porte la liste des onglets ouverts
/// plutôt qu'un unique document (voir `DocumentTab`), ainsi que ce qui est
/// réellement global à l'application : le backend GPU partagé (un seul
/// `Device`/`Queue` `wgpu`, cloné dans chaque onglet à sa création) et la
/// barre de menus native (une seule pour toute l'application, ses commandes
/// sont routées vers l'onglet actif).
struct ViewerApp {
    tabs: Vec<DocumentTab>,
    /// Indice de l'onglet actuellement affiché dans `tabs` — toujours valide
    /// (`tabs` n'est jamais vide, voir `close_active_tab`).
    active: usize,
    gpu: Option<pdf_render_gpu::GpuRenderer>,
    /// Barre de menus native macOS (Sprint 11-12, sprint.md) — `None` sur
    /// les plateformes non macOS ou si l'installation a échoué, auquel cas
    /// seuls la barre d'outils `egui` et le glisser-déposer restent
    /// utilisables pour ouvrir un fichier.
    native_menu: Option<NativeMenu>,
}

impl ViewerApp {
    fn new(initial_path: Option<PathBuf>, gpu: Option<pdf_render_gpu::GpuRenderer>) -> Self {
        Self {
            tabs: vec![DocumentTab::new(initial_path, gpu.clone())],
            active: 0,
            gpu,
            native_menu: None,
        }
    }

    /// Ouvre `path` dans un **nouvel** onglet et le rend actif — jamais dans
    /// l'onglet actuellement affiché (voir la doc de module sur
    /// `DocumentTabOutcome`).
    fn open_new_tab(&mut self, path: PathBuf) {
        let tab = DocumentTab::new(Some(path), self.gpu.clone());
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    /// "Fichier > Fermer l'onglet" (`⌘W`, Sprint 49) : ferme l'onglet actif.
    /// Si c'était le dernier, le remplace par un onglet vide plutôt que de
    /// fermer la fenêtre — `tabs` ne doit jamais être vide (simplification
    /// délibérée : fermer la fenêtre elle-même reste possible via `⇧⌘W`,
    /// "Fermer la fenêtre", `performClose:`).
    fn close_active_tab(&mut self) {
        self.tabs.remove(self.active);
        if self.tabs.is_empty() {
            self.tabs.push(DocumentTab::new(None, self.gpu.clone()));
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    /// Ferme l'onglet `index` (bouton "×" de la barre d'onglets) — même
    /// logique que `close_active_tab`, mais ajuste `active` pour continuer
    /// à pointer sur le même onglet visuellement quand celui fermé était
    /// avant lui dans la liste.
    fn close_tab(&mut self, index: usize) {
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(DocumentTab::new(None, self.gpu.clone()));
            self.active = 0;
        } else if index < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    /// Barre d'onglets (Sprint 49) : un onglet par document ouvert (titre =
    /// nom de fichier), un bouton "×" pour le fermer, et un bouton "+" pour
    /// en ouvrir un nouveau. Toujours affichée, même avec un seul onglet —
    /// évite un changement de disposition surprenant dès qu'un deuxième
    /// onglet s'ouvre.
    fn show_tab_bar(&mut self, ctx: &egui::Context) {
        let mut switch_to = None;
        let mut close_index = None;
        let mut new_tab_path = None;

        egui::TopBottomPanel::top("tab_bar").show(ctx, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("tab_bar_scroll")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (index, tab) in self.tabs.iter().enumerate() {
                            ui.group(|ui| {
                                if ui
                                    .selectable_label(index == self.active, tab.title())
                                    .clicked()
                                {
                                    switch_to = Some(index);
                                }
                                if ui
                                    .small_button("×")
                                    .on_hover_text("Fermer l'onglet")
                                    .clicked()
                                {
                                    close_index = Some(index);
                                }
                            });
                        }
                        if ui.button("+").on_hover_text("Nouvel onglet").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("PDF", &["pdf"])
                                .pick_file()
                            {
                                new_tab_path = Some(path);
                            }
                        }
                    });
                });
        });

        if let Some(index) = switch_to {
            self.active = index;
        }
        if let Some(index) = close_index {
            self.close_tab(index);
        }
        if let Some(path) = new_tab_path {
            self.open_new_tab(path);
        }
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Installée ici (première frame seulement) plutôt que dans le
        // callback de création de `main` : à ce stade, la boucle
        // d'événements `winit`/AppKit a fini son propre démarrage (qui
        // installe un menu par défaut), donc la nôtre ne se fait plus
        // écraser — voir la doc de `main` et `NativeMenu::install`.
        if self.native_menu.is_none() {
            if let Some(mtm) = objc2::MainThreadMarker::new() {
                self.native_menu = Some(NativeMenu::install(mtm));
            }
        }

        // Commandes émises par la barre de menus native depuis la dernière
        // frame (voir `native_menu.rs`) — traitées ici plutôt que dans le
        // callback Objective-C lui-même, pour ne coupler `MenuTarget` qu'à
        // un canal MPSC plutôt qu'à l'état `egui`. Routées vers l'onglet
        // actif, sauf `OpenDocument`/`CloseTab` qui agissent sur la liste
        // d'onglets elle-même (Sprint 49).
        if let Some(menu) = &self.native_menu {
            for cmd in menu.drain_commands() {
                match cmd {
                    MenuCommand::OpenDocument => {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("PDF", &["pdf"])
                            .pick_file()
                        {
                            self.open_new_tab(path);
                        }
                    }
                    MenuCommand::CloseTab => self.close_active_tab(),
                    MenuCommand::ExportCopyAs => self.tabs[self.active].export_copy_as(),
                    MenuCommand::ToggleDarkMode => {
                        if let Some(mtm) = objc2::MainThreadMarker::new() {
                            let dark = native_menu::toggle_dark_mode(mtm);
                            ctx.set_visuals(if dark {
                                egui::Visuals::dark()
                            } else {
                                egui::Visuals::light()
                            });
                        }
                    }
                    MenuCommand::Save => self.tabs[self.active].save_in_place(),
                    MenuCommand::Undo => self.tabs[self.active].undo_edit(),
                    MenuCommand::Redo => self.tabs[self.active].redo_edit(),
                    MenuCommand::Print => self.tabs[self.active].print_document(),
                }
            }
        }

        // Glisser-déposer un fichier PDF sur la fenêtre (`egui` expose déjà
        // les fichiers déposés via l'événement natif `NSWindow`/`winit` sans
        // code Objective-C supplémentaire) : ouvre un nouvel onglet, comme
        // "Ouvrir…" (Sprint 49) plutôt que de remplacer l'onglet actif.
        let dropped_path = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(path) = dropped_path {
            self.open_new_tab(path);
        }

        self.show_tab_bar(ctx);

        let outcome = self.tabs[self.active].update_content(ctx);
        if let Some(path) = outcome.open_in_new_tab {
            self.open_new_tab(path);
        }
    }
}
