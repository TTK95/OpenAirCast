//! Settings / Einstellungen.
//!
//! The groups whose contract really exists, in the order section 7.6 of the
//! design specification lists them: Appearance, Language, Keyboard, Advanced
//! information, About. Behavior and Connection are absent because no typed
//! shell event or effect backs them yet; announcing them with a dead switch
//! would be a promise this build cannot keep.
//!
//! Two rules shape the controls here:
//!
//! * **The announced name carries its group.** Appearance and Language both
//!   offer a choice painted "System". A screen reader that hears "System"
//!   twice on one page cannot tell which group it belongs to, so every radio
//!   announces `"<group>: <choice>"` while the painted label stays the short
//!   word.
//! * **A selection reads the preference, not the resolution.** Language
//!   selects on `locale_preference`; the copy around it is resolved through
//!   `resolved_locale`. Following System into German must not move the dot
//!   onto "Deutsch" -- the user never chose German, and moving it would make
//!   the way back to System invisible.
//!
//! The Apply-shortcut command is deliberately drawn quiet. The single filled
//! action of a destination is arbitrated once, by
//! [`crate::ui::presentation::filled_action_owner`], between the lifecycle
//! command and the Shell's Apply; a form-scoped commit that made itself
//! filled would put a second filled button on the page whenever staged
//! membership is dirty.

use egui::{WidgetInfo, WidgetType};

use crate::app::{AppEvent, HotkeyBinding, LocalePreference, ThemePreference, UiSnapshot};
use crate::ui::components::{command_action, switch};
use crate::ui::i18n::{Catalog, NamedValueArgs, TextKey};
use crate::ui::layout::UiResources;
use crate::ui::presentation::{
    CommandActionModel, Emphasis, SwitchModel, CONTROL_MIN_HEIGHT, DENSE_PADDING, SECTION_GAP,
};
use crate::ui::theme::TypographyRole;

/// The documents the About group offers, embedded so the open and copy
/// commands hand over the real text rather than a path the user then has to
/// find. No build or user path exists at runtime to leak.
const LICENSE_TEXT: &str = include_str!("../../../../../LICENSE");
const NOTICES_TEXT: &str = include_str!("../../../../../THIRD_PARTY_NOTICES.md");

const MOD_SHIFT: u32 = 0x0004;
const MOD_WIN: u32 = 0x0008;

/// Lowest and highest virtual key code a chord may use. The catalog states
/// this range to the user, so the validator has to enforce exactly it.
const KEY_CODE_RANGE: std::ops::RangeInclusive<u32> = 1..=254;

/// How wide the key-code field is drawn. Three digits and their caret.
const KEY_FIELD_WIDTH: f32 = 72.0;

/// How tall the in-app document viewer is before it scrolls.
const VIEWER_HEIGHT: f32 = 240.0;

/// Renders the Settings destination.
pub fn show(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    // Escape is arbitrated once, here. While the About viewer is open it
    // belongs to the viewer; only when there is no viewer does the chord
    // draft get it. Without this, one key press would silently do two things.
    let mut viewer = read_temp::<Option<AboutDocument>>(ui, viewer_id()).flatten();
    let mut receipt = read_temp::<Option<AboutDocument>>(ui, receipt_id()).flatten();
    let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let escape_for_chord = if escape && viewer.is_some() {
        viewer = None;
        false
    } else {
        escape
    };

    group_card(ui, resources, |card| {
        appearance_group(card, snapshot, resources, emit)
    });
    ui.add_space(SECTION_GAP);
    group_card(ui, resources, |card| {
        language_group(card, snapshot, resources, emit)
    });
    ui.add_space(SECTION_GAP);
    group_card(ui, resources, |card| {
        keyboard_group(card, snapshot, resources, emit, escape_for_chord)
    });
    ui.add_space(SECTION_GAP);
    group_card(ui, resources, |card| {
        advanced_group(card, snapshot, resources, emit)
    });
    ui.add_space(SECTION_GAP);
    group_card(ui, resources, |card| {
        about_group(card, resources, &mut viewer, &mut receipt)
    });

    ui.memory_mut(|memory| {
        memory.data.insert_temp(viewer_id(), viewer);
        memory.data.insert_temp(receipt_id(), receipt);
    });
}

use super::group_card;

/// One choice of a Settings radio group.
///
/// The painted label is the short word; the announced name carries the group
/// so that two groups offering "System" stay distinguishable.
fn radio_choice(
    ui: &mut egui::Ui,
    catalog: Catalog,
    group: TextKey,
    option: TextKey,
    selected: bool,
) -> egui::Response {
    let response = ui.add(egui::RadioButton::new(selected, catalog.text(option)));
    let name = catalog.named_value(NamedValueArgs {
        name: catalog.text(group),
        value: catalog.text(option),
    });
    response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, &name));
    response
}

/// The title and explanation every group opens with.
fn group_heading(ui: &mut egui::Ui, resources: &UiResources<'_>, title: TextKey, body: TextKey) {
    super::group_heading(
        ui,
        resources,
        resources.catalog.text(title),
        resources.catalog.text(body),
    );
}

fn appearance_group(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let catalog = resources.catalog;
    group_heading(
        ui,
        resources,
        TextKey::Appearance,
        TextKey::AppearanceDescription,
    );

    ui.horizontal(|row| {
        for (preference, key) in [
            (ThemePreference::System, TextKey::ThemeSystem),
            (ThemePreference::Light, TextKey::ThemeLight),
            (ThemePreference::Dark, TextKey::ThemeDark),
        ] {
            let selected = snapshot.theme == preference;
            if radio_choice(row, catalog, TextKey::Appearance, key, selected).clicked() {
                emit(AppEvent::ThemeChanged(preference));
            }
        }
    });

    // High Contrast is a Windows setting, not a fourth preference: the
    // sentence explains where it lives and offers no control.
    super::show_text(
        ui,
        TypographyRole::Secondary,
        resources.tokens.ink_muted,
        catalog.text(TextKey::HighContrastControlledByWindows),
    );
}

fn language_group(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let catalog = resources.catalog;
    group_heading(
        ui,
        resources,
        TextKey::Language,
        TextKey::LanguageDescription,
    );

    ui.horizontal(|row| {
        for (preference, key) in [
            (LocalePreference::System, TextKey::LanguageSystem),
            (LocalePreference::German, TextKey::LanguageGerman),
            (LocalePreference::English, TextKey::LanguageEnglish),
        ] {
            // The preference, never the resolution.
            let selected = snapshot.locale_preference == preference;
            if radio_choice(row, catalog, TextKey::Language, key, selected).clicked() {
                emit(AppEvent::LocaleChanged(preference));
            }
        }
    });
}

/// The transient chord being edited. It lives in egui memory only and is
/// never domain state; Escape returns it to the applied binding.
///
/// The key is held as the text the user typed rather than as a number. A
/// numeric field that clamps its own input can never be wrong, which made
/// [`TextKey::ShortcutInvalidKey`] -- a sentence the catalog promises -- text
/// no input could reach.
///
/// What the text *says* is the key: `H`, `F5`, `7`. Windows files keys under
/// numbers, and the field used to hold that number under a label reading
/// "virtual key code", which is section 3 point 9 of the design
/// specification -- an implementation-near label -- on the one control the
/// group exists for. [`key_code`] reads the name back; a bare number is still
/// accepted, because a key outside the three named ranges has no name this
/// window could show and the number is then the honest thing to hold.
#[derive(Clone, Debug, PartialEq)]
struct HotkeyDraft {
    enabled: bool,
    control: bool,
    alt: bool,
    shift: bool,
    win: bool,
    key_text: String,
}

impl HotkeyDraft {
    fn from_binding(binding: HotkeyBinding) -> Self {
        Self {
            enabled: binding.enabled,
            control: binding.modifiers & crate::app::MOD_CONTROL != 0,
            alt: binding.modifiers & crate::app::MOD_ALT != 0,
            shift: binding.modifiers & MOD_SHIFT != 0,
            win: binding.modifiers & MOD_WIN != 0,
            key_text: key_label(binding.virtual_key),
        }
    }

    fn modifiers(&self) -> u32 {
        let mut modifiers = 0;
        if self.control {
            modifiers |= crate::app::MOD_CONTROL;
        }
        if self.alt {
            modifiers |= crate::app::MOD_ALT;
        }
        if self.shift {
            modifiers |= MOD_SHIFT;
        }
        if self.win {
            modifiers |= MOD_WIN;
        }
        modifiers
    }

    /// The key the text stands for, if it names or spells one at all.
    ///
    /// The name wins over the number, which costs the single digits `1`..`9`
    /// their reading as codes: typing `5` means the 5 key (`VK_5`, 0x35) and
    /// never `VK_0x05`. That is what a person typing into a key field means,
    /// and the codes below 0x0A are mouse buttons and `VK_CANCEL` -- with
    /// Backspace and Tab, which stay reachable as `08` and `09` because a
    /// name is exactly one character long.
    fn virtual_key(&self) -> Option<u32> {
        let text = self.key_text.trim();
        key_code(text).or_else(|| text.parse::<u32>().ok())
    }

    /// The chord this draft stands for, or `None` while the key code is not a
    /// number.
    fn to_binding(&self) -> Option<HotkeyBinding> {
        Some(HotkeyBinding {
            enabled: self.enabled,
            modifiers: self.modifiers(),
            virtual_key: self.virtual_key()?,
        })
    }
}

/// Why a draft cannot be applied, as a catalog key.
///
/// The key code is checked first and regardless of `enabled`: a field holding
/// something that is not a key code is wrong whether or not the chord is
/// armed, and saying so is more useful than silently disabling Apply.
fn validation_error(draft: &HotkeyDraft) -> Option<TextKey> {
    match draft.virtual_key() {
        None => return Some(TextKey::ShortcutInvalidKey),
        Some(key) if !KEY_CODE_RANGE.contains(&key) => {
            return Some(TextKey::ShortcutInvalidKey);
        }
        Some(_) => {}
    }
    if draft.enabled && draft.modifiers() == 0 {
        return Some(TextKey::ShortcutNeedsModifier);
    }
    None
}

fn hotkey_draft_id() -> egui::Id {
    egui::Id::new("openaircast_shell_hotkey_draft")
}

fn viewer_id() -> egui::Id {
    egui::Id::new("openaircast_settings_about_viewer")
}

fn receipt_id() -> egui::Id {
    egui::Id::new("openaircast_settings_about_receipt")
}

fn read_temp<T: Clone + Send + Sync + 'static>(ui: &egui::Ui, id: egui::Id) -> Option<T> {
    ui.memory_mut(|memory| memory.data.get_temp::<T>(id))
}

fn keyboard_group(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
    escape_pressed: bool,
) {
    let catalog = resources.catalog;
    let tokens = resources.tokens;
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        tokens.ink,
        catalog.text(TextKey::Keyboard),
    );
    // One heading, not two. "Keyboard" and "Global shortcut" used to be set
    // one directly under the other, which read as two section titles for one
    // section; the group is named once and the shortcut names itself where it
    // explains itself.
    super::show_text(
        ui,
        TypographyRole::Body,
        tokens.ink_muted,
        &catalog.named_value(NamedValueArgs {
            name: catalog.text(TextKey::GlobalShortcut),
            value: catalog.text(TextKey::GlobalShortcutDescription),
        }),
    );

    let applied = snapshot.hotkey;
    let mut draft = read_temp::<HotkeyDraft>(ui, hotkey_draft_id())
        .unwrap_or_else(|| HotkeyDraft::from_binding(applied));

    // Escape discards only this transient edit; staged membership is
    // untouched, and the About viewer has already had its turn.
    if escape_pressed && draft.to_binding() != Some(applied) {
        draft = HotkeyDraft::from_binding(applied);
    }

    ui.horizontal(|row| {
        for (flag, key) in [
            (&mut draft.enabled, TextKey::ShortcutEnabled),
            (&mut draft.control, TextKey::ShortcutControl),
            (&mut draft.alt, TextKey::ShortcutAlt),
            (&mut draft.shift, TextKey::ShortcutShift),
            (&mut draft.win, TextKey::ShortcutWindows),
        ] {
            row.checkbox(flag, catalog.text(key));
        }
    });

    ui.horizontal(|row| {
        super::show_text(
            row,
            TypographyRole::Body,
            tokens.ink,
            catalog.text(TextKey::ShortcutKey),
        );
        let accessible = catalog.text(TextKey::ShortcutKeyAccessibility);
        // The field is padded to the shared control height so it is the same
        // 40-point target as every other control on the page.
        let row_height = row.text_style_height(&egui::TextStyle::Body);
        let padding = (((CONTROL_MIN_HEIGHT - row_height) / 2.0).ceil()).max(0.0) as i8;
        let field = row.add(
            egui::TextEdit::singleline(&mut draft.key_text)
                .desired_width(KEY_FIELD_WIDTH)
                .char_limit(3)
                .margin(egui::Margin::symmetric(8, padding)),
        );
        let value = draft.key_text.clone();
        field.widget_info(|| {
            let mut info = WidgetInfo::text_edit(true, &value, &value, "");
            info.label = Some(accessible.to_owned());
            info
        });
        field.on_hover_text(catalog.text(TextKey::ShortcutKeyTooltip));
    });

    let candidate = draft.to_binding();
    let error = validation_error(&draft);
    let changed = candidate.is_some_and(|binding| binding != applied);

    let apply = CommandActionModel::new(
        catalog.text(TextKey::ApplyShortcut),
        Emphasis::Quiet,
        error.is_none() && changed,
    );
    let activated = command_action::show(ui, tokens, &apply).clicked();
    if let (true, Some(binding)) = (activated, candidate) {
        emit(AppEvent::HotkeyChanged(binding));
    }

    if let Some(key) = error {
        super::show_text(ui, TypographyRole::Body, tokens.fault, catalog.text(key));
    } else if changed {
        super::show_text(
            ui,
            TypographyRole::Body,
            tokens.ink_muted,
            catalog.text(TextKey::ShortcutChangePending),
        );
    }

    super::show_text(
        ui,
        TypographyRole::Body,
        tokens.ink_muted,
        &catalog.named_value(NamedValueArgs {
            name: catalog.text(TextKey::ActiveShortcut),
            value: &describe_binding(applied, catalog),
        }),
    );

    ui.memory_mut(|memory| memory.data.insert_temp(hotkey_draft_id(), draft));
}

/// The key a virtual key code stands for, where its name is the same in
/// every language.
///
/// Windows numbers its keys, and the field the user types into holds that
/// number -- which is what the catalog promises it contains. What must not
/// happen is the *applied* chord reporting itself as "Ctrl+Alt+72": a
/// sentence in which nothing names a key the user could press. Section 3
/// point 9 of the design specification calls that out by name as
/// implementation-near labelling.
///
/// The table is deliberately confined to the three ranges whose Windows
/// constants are their own name in any language: `VK_0`..`VK_9` (0x30..0x39),
/// `VK_A`..`VK_Z` (0x41..0x5A), and `VK_F1`..`VK_F24` (0x70..0x87). A named
/// key -- Space, Enter, Escape -- would need catalog copy per language, and
/// printing the English word inside a German window would be the very defect
/// this function exists to remove. Everything outside the ranges keeps its
/// number, which is honest: the window does not know what that key is called.
fn key_name(virtual_key: u32) -> Option<String> {
    match virtual_key {
        0x30..=0x39 => Some(char::from(b'0' + (virtual_key - 0x30) as u8).to_string()),
        0x41..=0x5a => Some(char::from(b'A' + (virtual_key - 0x41) as u8).to_string()),
        0x70..=0x87 => Some(format!("F{}", virtual_key - 0x70 + 1)),
        _ => None,
    }
}

/// The key's name, or its code where the window has no name for it.
/// The label a key carries: its name where it has one, its number otherwise.
///
/// A one-digit number is written with a leading zero. Not decoration: the
/// field the user edits holds this text and [`key_code`] reads it back, and a
/// bare `5` reads back as the 5 key -- so an unpadded label would show the
/// user one key and apply another the next time the draft was read.
fn key_label(virtual_key: u32) -> String {
    key_name(virtual_key).unwrap_or_else(|| format!("{virtual_key:02}"))
}

/// The virtual key code a typed name stands for.
///
/// The exact inverse of [`key_name`] over the three named ranges, and `None`
/// for everything else, so the field can hold what the window shows. Letters
/// are matched without regard to case: a user typing the shortcut key types
/// it the way it sits on the keyboard, not the way Windows capitalizes its
/// constants.
fn key_code(name: &str) -> Option<u32> {
    let name = name.trim();
    let mut characters = name.chars();
    match (characters.next(), characters.next()) {
        (Some(single), None) if single.is_ascii_digit() => {
            Some(0x30 + u32::from(single as u8 - b'0'))
        }
        (Some(single), None) if single.is_ascii_alphabetic() => {
            Some(0x41 + u32::from(single.to_ascii_uppercase() as u8 - b'A'))
        }
        (Some('F' | 'f'), Some(_)) => {
            let index = name[1..].parse::<u32>().ok()?;
            (1..=24).contains(&index).then(|| 0x70 + index - 1)
        }
        _ => None,
    }
}

/// The applied chord in the user's language, with its key named rather than
/// numbered wherever the window knows the name.
fn describe_binding(binding: HotkeyBinding, catalog: Catalog) -> String {
    if !binding.enabled {
        return catalog.text(TextKey::ShortcutDisabled).to_owned();
    }
    let mut parts: Vec<&str> = Vec::new();
    for (mask, key) in [
        (crate::app::MOD_CONTROL, TextKey::ShortcutControl),
        (crate::app::MOD_ALT, TextKey::ShortcutAlt),
        (MOD_SHIFT, TextKey::ShortcutShift),
        (MOD_WIN, TextKey::ShortcutWindows),
    ] {
        if binding.modifiers & mask != 0 {
            parts.push(catalog.text(key));
        }
    }
    let modifiers = parts.join("+");
    let key = key_label(binding.virtual_key);
    if modifiers.is_empty() {
        return key;
    }
    format!("{modifiers}+{key}")
}

/// The Advanced-information switch.
///
/// It is not decoration: `advanced_information` is what puts the applied
/// session membership on every receiver row and the measurement strip on
/// Overview. The painted word and the reported toggle state both come from
/// what this frame draws, so the announcement can never disagree with the
/// picture.
fn advanced_group(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let catalog = resources.catalog;
    group_heading(
        ui,
        resources,
        TextKey::AdvancedInformation,
        TextKey::AdvancedInformationDescription,
    );

    let applied = snapshot.advanced_information;
    // The track carries no painted text of its own: its accessible name is
    // the setting, and the state word travels beside it as a text node, so
    // the announced name stays stable while the visible word tells the state
    // without relying on the track's fill colour.
    //
    // Both the word and the announced state come from the snapshot, never
    // from an optimistic local copy: the previous control let egui flip its
    // own tick on the click frame, which is a picture of a state no reducer
    // had accepted yet.
    let model = SwitchModel::new(
        catalog.text(TextKey::AdvancedInformation),
        catalog.text(if applied { TextKey::On } else { TextKey::Off }),
        applied,
    );
    switch::show(
        ui,
        resources.tokens,
        &model,
        AppEvent::AdvancedInformationChanged(!applied),
        emit,
    );
}

/// Which embedded document the in-app viewer is showing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AboutDocument {
    License,
    Notices,
}

impl AboutDocument {
    /// The embedded text. No path is involved at any point.
    const fn text(self) -> &'static str {
        match self {
            Self::License => LICENSE_TEXT,
            Self::Notices => NOTICES_TEXT,
        }
    }

    const fn title(self) -> TextKey {
        match self {
            Self::License => TextKey::License,
            Self::Notices => TextKey::ThirdPartyNotices,
        }
    }

    const fn open_label(self) -> TextKey {
        match self {
            Self::License => TextKey::OpenLicense,
            Self::Notices => TextKey::OpenThirdPartyNotices,
        }
    }

    const fn copy_label(self) -> TextKey {
        match self {
            Self::License => TextKey::CopyLicenseText,
            Self::Notices => TextKey::CopyNoticesText,
        }
    }

    /// The scroll position belongs to the document, not to the viewer.
    ///
    /// egui keeps a `ScrollArea`'s offset in context memory under the id its
    /// salt produces. One salt for both documents would therefore carry the
    /// place reached in the long licence straight over into the short
    /// notices, which open somewhere in their middle -- or, once the offset
    /// is clamped, at their end -- with nothing on screen saying that the
    /// beginning was skipped. A salt per document gives each one its own
    /// place, so switching always starts at the top of the new text.
    const fn scroll_salt(self) -> &'static str {
        match self {
            Self::License => "openaircast_settings_about_viewer_scroll_license",
            Self::Notices => "openaircast_settings_about_viewer_scroll_notices",
        }
    }
}

const ABOUT_DOCUMENTS: [AboutDocument; 2] = [AboutDocument::License, AboutDocument::Notices];

fn about_group(
    ui: &mut egui::Ui,
    resources: &UiResources<'_>,
    viewer: &mut Option<AboutDocument>,
    receipt: &mut Option<AboutDocument>,
) {
    let catalog = resources.catalog;
    let tokens = resources.tokens;
    group_heading(ui, resources, TextKey::About, TextKey::AboutDescription);
    super::show_text(
        ui,
        TypographyRole::Body,
        tokens.ink,
        &catalog.named_value(NamedValueArgs {
            name: catalog.text(TextKey::Version),
            value: env!("CARGO_PKG_VERSION"),
        }),
    );

    // One row per document: the document is named, and its two commands sit
    // with it, so neither command has to say which file it means twice.
    for document in ABOUT_DOCUMENTS {
        super::show_text(
            ui,
            TypographyRole::Body,
            tokens.ink,
            catalog.text(document.title()),
        );
        ui.horizontal(|row| {
            let open =
                CommandActionModel::new(catalog.text(document.open_label()), Emphasis::Quiet, true);
            if command_action::show(row, tokens, &open).clicked() {
                *viewer = Some(document);
                // A new command supersedes the previous receipt.
                *receipt = None;
            }
            let copy =
                CommandActionModel::new(catalog.text(document.copy_label()), Emphasis::Quiet, true);
            if command_action::show(row, tokens, &copy).clicked() {
                row.ctx().copy_text(document.text().to_owned());
                *receipt = Some(document);
            }
        });
    }

    // The receipt for the last copy. It is not a timed toast: a renderer
    // here has no clock to read, and a confirmation that vanishes on a timer
    // could not be verified without one. It stands until another About
    // command replaces it.
    if receipt.is_some() {
        super::show_text(
            ui,
            TypographyRole::Secondary,
            tokens.ink_muted,
            catalog.text(TextKey::CopiedToClipboard),
        );
    }

    ui.add_space(DENSE_PADDING);
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        tokens.ink,
        catalog.text(TextKey::SupportProject),
    );
    super::show_text(
        ui,
        TypographyRole::Body,
        tokens.ink_muted,
        catalog.text(TextKey::SupportProjectDescription),
    );
    let support = CommandActionModel::new(
        catalog.text(TextKey::SupportWithPaypal),
        Emphasis::Quiet,
        true,
    );
    if command_action::show(ui, tokens, &support).clicked() {
        ui.ctx()
            .open_url(egui::OpenUrl::new_tab("https://paypal.me/ttk95"));
    }
    super::show_text(
        ui,
        TypographyRole::Secondary,
        tokens.ink_muted,
        catalog.text(TextKey::SupportBrowserHint),
    );

    if let Some(document) = *viewer {
        ui.add_space(DENSE_PADDING);
        super::show_text(
            ui,
            TypographyRole::SectionTitle,
            tokens.ink,
            catalog.text(document.title()),
        );
        let close = CommandActionModel::new(
            catalog.text(TextKey::CloseAboutViewer),
            Emphasis::Quiet,
            true,
        );
        if command_action::show(ui, tokens, &close).clicked() {
            *viewer = None;
        }
        egui::ScrollArea::vertical()
            .id_salt(document.scroll_salt())
            .max_height(VIEWER_HEIGHT)
            .show(ui, |body| {
                super::show_text(body, TypographyRole::Body, tokens.ink, document.text());
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ResolvedLocale;

    mod key_names {
        use super::*;

        /// The three ranges the table covers, each at both ends.
        #[test]
        fn a_letter_a_digit_and_a_function_key_are_named() {
            assert_eq!(key_name(0x48).as_deref(), Some("H"));
            assert_eq!(key_name(0x41).as_deref(), Some("A"));
            assert_eq!(key_name(0x5a).as_deref(), Some("Z"));
            assert_eq!(key_name(0x30).as_deref(), Some("0"));
            assert_eq!(key_name(0x39).as_deref(), Some("9"));
            assert_eq!(key_name(0x70).as_deref(), Some("F1"));
            assert_eq!(key_name(0x87).as_deref(), Some("F24"));
        }

        /// Reading a name back is the exact inverse of writing it, over the
        /// whole code space -- otherwise the field would show one key and
        /// apply another.
        #[test]
        fn every_named_key_reads_back_as_the_code_it_was_named_for() {
            for code in 0..=0xffu32 {
                match key_name(code) {
                    Some(name) => assert_eq!(
                        key_code(&name),
                        Some(code),
                        "{code:#04x} is shown as {name:?} and read back as something else"
                    ),
                    None => assert_eq!(
                        key_code(&key_label(code)),
                        None,
                        "{code:#04x} has no name, so its label must not parse as one"
                    ),
                }
            }
        }

        /// Case is what the keyboard shows, not what Windows capitalizes.
        #[test]
        fn a_key_typed_in_either_case_reads_as_the_same_code() {
            for (typed, expected) in [("h", 0x48u32), ("H", 0x48), ("f5", 0x74), ("F5", 0x74)] {
                assert_eq!(key_code(typed), Some(expected), "{typed}");
            }
        }

        /// A single digit is the digit key, and nothing outside the three
        /// ranges is invented into one.
        #[test]
        fn only_the_three_named_ranges_parse_as_names() {
            assert_eq!(key_code("5"), Some(0x35), "a typed 5 is the 5 key");
            for typed in ["", " ", "F0", "F25", "AB", "Space", "72", "08", "-1"] {
                assert_eq!(key_code(typed), None, "{typed:?} was read as a key name");
            }
        }

        /// A key the window has no name for keeps its number rather than
        /// becoming an empty label.
        #[test]
        fn an_unnamed_key_falls_back_to_its_code_and_never_to_nothing() {
            for code in [0x01u32, 0x20, 0x2e, 0x6f, 0x88, 0xfe] {
                assert_eq!(key_name(code), None, "{code:#04x} claims a name");
                let label = key_label(code);
                assert_eq!(
                    label.trim_start_matches('0'),
                    code.to_string(),
                    "{code:#04x}"
                );
                assert!(!label.trim().is_empty(), "{code:#04x} labels nothing");
            }
        }

        /// The chord a user reads names its key in both directions: a known
        /// code as the key, an unknown one as the number the field holds.
        #[test]
        fn the_applied_chord_names_a_known_key_and_numbers_an_unknown_one() {
            for locale in [ResolvedLocale::German, ResolvedLocale::English] {
                let catalog = Catalog::new(locale);
                let known = HotkeyBinding {
                    enabled: true,
                    modifiers: crate::app::MOD_CONTROL | crate::app::MOD_ALT,
                    virtual_key: 0x48,
                };
                assert_eq!(describe_binding(known, catalog), "Ctrl+Alt+H", "{locale:?}");

                let unknown = HotkeyBinding {
                    virtual_key: 0x20,
                    ..known
                };
                assert_eq!(
                    describe_binding(unknown, catalog),
                    "Ctrl+Alt+32",
                    "{locale:?}"
                );

                let bare = HotkeyBinding {
                    modifiers: 0,
                    ..known
                };
                assert_eq!(describe_binding(bare, catalog), "H", "{locale:?}");

                let off = HotkeyBinding {
                    enabled: false,
                    ..known
                };
                assert_eq!(
                    describe_binding(off, catalog),
                    catalog.text(TextKey::ShortcutDisabled),
                    "{locale:?}"
                );
            }
        }
    }
}
