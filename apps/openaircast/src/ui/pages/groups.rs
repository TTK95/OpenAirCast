//! Saved groups. The editor lives in page-local egui memory, never in selection state.

use egui::WidgetInfo;

use crate::app::{
    AppEvent, GroupCommand, GroupFailure, GroupId, GroupMember, GroupOperation,
    GroupOperationStatus, SavedGroupReading, UiSnapshot,
};
use crate::ui::components::command_action;
use crate::ui::i18n::TextKey;
use crate::ui::layout::UiResources;
use crate::ui::presentation::{CommandActionModel, Emphasis, SECTION_GAP};
use crate::ui::theme::TypographyRole;

#[derive(Clone, Debug, PartialEq)]
struct GroupDraft {
    id: Option<GroupId>,
    name: String,
    members: Vec<GroupMember>,
    candidates: Vec<GroupMember>,
    submitted: Option<GroupCommand>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupDraftValidation {
    NameRequired,
    MemberRequired,
    Duplicate,
}
impl GroupDraft {
    fn new() -> Self {
        Self {
            id: None,
            name: String::new(),
            members: Vec::new(),
            candidates: Vec::new(),
            submitted: None,
        }
    }
    fn from_group(group: &SavedGroupReading) -> Self {
        Self {
            id: Some(group.id),
            name: group.name.clone(),
            members: group.members.clone(),
            candidates: group.members.clone(),
            submitted: None,
        }
    }
    fn validation(&self) -> Option<GroupDraftValidation> {
        if self.name.trim().is_empty() {
            Some(GroupDraftValidation::NameRequired)
        } else if self.members.is_empty() {
            Some(GroupDraftValidation::MemberRequired)
        } else {
            None
        }
    }
    fn set_member_selected(&mut self, id: &airplay_core::DeviceId, name: &str, selected: bool) {
        if selected {
            if !self.members.iter().any(|member| &member.receiver == id) {
                let member = self
                    .candidates
                    .iter()
                    .find(|member| &member.receiver == id)
                    .cloned()
                    .unwrap_or_else(|| GroupMember {
                        receiver: id.clone(),
                        name: name.into(),
                        level: 1.0,
                    });
                self.members.push(member);
            }
        } else {
            if let Some(member) = self
                .members
                .iter()
                .find(|member| &member.receiver == id)
                .cloned()
            {
                self.candidates
                    .retain(|candidate| &candidate.receiver != id);
                self.candidates.push(member);
            }
            self.members.retain(|member| &member.receiver != id);
        }
    }

    fn candidates(&self, snapshot: &UiSnapshot) -> Vec<(airplay_core::DeviceId, String, bool)> {
        let mut candidates: Vec<_> = snapshot
            .receivers
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    r.name.clone(),
                    r.availability != crate::app::Availability::Available,
                )
            })
            .collect();
        candidates.extend(
            self.candidates
                .iter()
                .filter(|m| !snapshot.receivers.iter().any(|r| r.id == m.receiver))
                .map(|m| (m.receiver.clone(), m.name.clone(), true)),
        );
        // Also retain a newly selected discovered receiver if it goes offline
        // while this editor remains open.
        for member in &self.members {
            if !candidates.iter().any(|(id, _, _)| id == &member.receiver) {
                candidates.push((member.receiver.clone(), member.name.clone(), true));
            }
        }
        candidates
    }
    fn command(&self) -> GroupCommand {
        GroupCommand::Save {
            id: self.id,
            name: self.name.trim().to_owned(),
            members: self.members.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct PageState {
    draft: Option<GroupDraft>,
    delete: Option<SavedGroupReading>,
    apply_choice: Option<(GroupId, bool)>,
    sent: Option<GroupCommand>,
    sent_after: Option<u64>,
    associated_request: Option<u64>,
    observed_operation: Option<GroupOperation>,
    acknowledged_request: Option<u64>,
    admission_failure: Option<GroupFailure>,
}
impl PageState {
    fn submit(&mut self, snapshot: &UiSnapshot, command: GroupCommand) {
        self.sent = Some(command);
        self.sent_after = snapshot.group_operation.as_ref().map(|op| op.request);
        self.associated_request = None;
        self.observed_operation = None;
        self.acknowledged_request = None;
        self.admission_failure = None;
    }
    fn open(&mut self, draft: GroupDraft) {
        self.draft = Some(draft);
        self.sent = None;
        self.associated_request = None;
        self.observed_operation = None;
        self.admission_failure = None;
    }
    fn confirmation_lost(&mut self) {
        self.sent = None;
        self.associated_request = None;
        self.observed_operation = None;
        self.admission_failure = Some(GroupFailure::ConfirmationLost);
    }
    fn blocked(&self, snapshot: &UiSnapshot) -> bool {
        snapshot.shutting_down
            || is_pending(snapshot)
            || (self.sent.is_some() && self.associated_request.is_none())
    }
    fn can_submit(&self, snapshot: &UiSnapshot) -> bool {
        self.admission_failure != Some(GroupFailure::Closed)
            && !snapshot
                .group_operation
                .as_ref()
                .is_some_and(|op| op.status == GroupOperationStatus::Failed(GroupFailure::Closed))
    }
}

/// Records bounded shell admission failure separately from reducer outcomes.
/// The page's local state is written after emit returns, so use a separate slot.
pub(crate) fn admission_failed(ctx: &egui::Context, failure: crate::app_handle::AppUnavailable) {
    let failure = match failure {
        crate::app_handle::AppUnavailable::Busy => GroupFailure::Busy,
        crate::app_handle::AppUnavailable::Closed => GroupFailure::Closed,
    };
    ctx.data_mut(|data| data.insert_temp(state_id().with("admission"), Some(failure)));
    ctx.request_repaint();
}
fn state_id() -> egui::Id {
    egui::Id::new("openaircast_groups_page_state")
}
fn read_state(ui: &egui::Ui) -> PageState {
    ui.memory_mut(|m| m.data.get_temp(state_id()))
        .unwrap_or_default()
}
fn is_pending(snapshot: &UiSnapshot) -> bool {
    snapshot
        .group_operation
        .as_ref()
        .is_some_and(|op| op.status == GroupOperationStatus::Pending)
}
fn action(ui: &mut egui::Ui, resources: &UiResources<'_>, key: TextKey, enabled: bool) -> bool {
    command_action::show(
        ui,
        resources.tokens,
        &CommandActionModel::new(resources.catalog.text(key), Emphasis::Quiet, enabled),
    )
    .clicked()
}

/// Renders confirmed saved groups and the independent create/edit draft.
pub fn show(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let mut state = read_state(ui);
    if let Some(failure) = ui
        .ctx()
        .data_mut(|data| data.remove_temp::<Option<GroupFailure>>(state_id().with("admission")))
        .flatten()
    {
        state.admission_failure = Some(failure);
        state.sent = None;
        state.associated_request = None;
        state.observed_operation = None;
    }
    associate_outcome(&mut state, snapshot);
    match snapshot.saved_groups.as_deref() {
        None => {
            super::show_text(
                ui,
                TypographyRole::Body,
                resources.tokens.ink_muted,
                resources.catalog.text(TextKey::GroupsLoading),
            );
        }
        Some(groups) if state.draft.is_some() => {
            feedback(ui, snapshot, resources, &mut state);
            editor(ui, snapshot, resources, groups, &mut state, emit);
        }
        Some(groups) => list(ui, snapshot, resources, groups, &mut state, emit),
    }
    ui.memory_mut(|m| m.data.insert_temp(state_id(), state));
}

fn associate_outcome(state: &mut PageState, snapshot: &UiSnapshot) {
    if let Some(observed) = state.observed_operation.as_ref() {
        if snapshot.group_operation.as_ref().map(|op| op.request) != Some(observed.request) {
            // The global slot is shared with selection apply. Its replacement
            // proves nothing about our command's outcome. Retain an exact
            // terminal result already seen; otherwise report uncertainty.
            if observed.status == GroupOperationStatus::Pending {
                state.confirmation_lost();
            }
            return;
        }
    }
    let Some(operation) = snapshot.group_operation.as_ref() else {
        return;
    };
    if state.sent.is_none() || state.sent_after == Some(operation.request) {
        return;
    }
    if state.associated_request.is_none() {
        if state.sent.as_ref() != Some(&operation.command) {
            // Our save may have completed while another page was visible.
            // An unrelated command is never evidence that it was refused.
            state.confirmation_lost();
            return;
        }
        state.associated_request = Some(operation.request);
    }
    state.observed_operation = Some(operation.clone());
    if state.associated_request == Some(operation.request)
        && operation.status == GroupOperationStatus::Succeeded
        && state
            .draft
            .as_ref()
            .and_then(|draft| draft.submitted.as_ref())
            == Some(&operation.command)
    {
        state.draft = None;
    }
}

fn list(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    groups: &[SavedGroupReading],
    state: &mut PageState,
    emit: &mut dyn FnMut(AppEvent),
) {
    let blocked = state.blocked(snapshot) || !state.can_submit(snapshot);
    if let Some(group) = state.delete.clone() {
        super::group_card(ui, resources, |card| {
            super::show_text(
                card,
                TypographyRole::SectionTitle,
                resources.tokens.ink,
                resources.catalog.text(TextKey::ConfirmDelete),
            );
            super::show_text(
                card,
                TypographyRole::Body,
                resources.tokens.ink_muted,
                &resources.catalog.group_delete_confirmation(&group.name),
            );
            card.horizontal(|row| {
                if action(row, resources, TextKey::Cancel, !blocked) {
                    state.delete = None;
                }
                if action(row, resources, TextKey::DeleteGroup, !blocked) {
                    let command = GroupCommand::Delete(group.id);
                    state.submit(snapshot, command.clone());
                    emit(AppEvent::GroupRequested(command));
                    state.delete = None;
                }
            });
        });
        return;
    }
    if let Some((id, start)) = state.apply_choice {
        return apply_conflict(ui, snapshot, resources, state, emit, id, start);
    }
    feedback(ui, snapshot, resources, state);
    if groups.is_empty() {
        super::group_card(ui, resources, |card| {
            super::group_heading(
                card,
                resources,
                resources.catalog.text(TextKey::GroupsEmptyTitle),
                resources.catalog.text(TextKey::GroupsEmptyBody),
            );
            if action(card, resources, TextKey::CreateGroup, !blocked) {
                state.open(GroupDraft::new());
            }
        });
        return;
    }
    if action(ui, resources, TextKey::CreateGroup, !blocked) {
        state.open(GroupDraft::new());
    }
    ui.add_space(SECTION_GAP);
    for group in groups {
        super::group_card(ui, resources, |card| {
            super::show_text(
                card,
                TypographyRole::SectionTitle,
                resources.tokens.ink,
                &group.name,
            );
            let available = group
                .members
                .iter()
                .filter(|member| group.available_members.contains(&member.receiver))
                .count();
            super::show_text(
                card,
                TypographyRole::Secondary,
                resources.tokens.ink_muted,
                &resources
                    .catalog
                    .group_availability(available, group.members.len()),
            );
            for member in &group.members {
                let suffix = (!group.available_members.contains(&member.receiver))
                    .then(|| resources.catalog.text(TextKey::GroupMemberOffline));
                super::show_text(
                    card,
                    TypographyRole::Body,
                    resources.tokens.ink,
                    &match suffix {
                        Some(s) => format!("{} · {s}", member.name),
                        None => member.name.clone(),
                    },
                );
            }
            card.horizontal_wrapped(|row| {
                if action(row, resources, TextKey::ApplyGroup, !blocked) {
                    request_apply(snapshot, state, group.id, false, emit);
                }
                if action(row, resources, TextKey::ApplyGroupAndStart, !blocked) {
                    request_apply(snapshot, state, group.id, true, emit);
                }
                if action(row, resources, TextKey::EditGroup, !blocked) {
                    state.open(GroupDraft::from_group(group));
                }
                if action(row, resources, TextKey::DeleteGroup, !blocked) {
                    state.delete = Some(group.clone());
                }
            });
        });
        ui.add_space(SECTION_GAP);
    }
}

fn request_apply(
    snapshot: &UiSnapshot,
    state: &mut PageState,
    id: GroupId,
    start: bool,
    emit: &mut dyn FnMut(AppEvent),
) {
    if snapshot.staged_membership_dirty {
        state.apply_choice = Some((id, start));
    } else {
        let command = GroupCommand::Apply { id, start };
        state.submit(snapshot, command.clone());
        emit(AppEvent::GroupRequested(command));
    }
}
fn apply_conflict(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    state: &mut PageState,
    emit: &mut dyn FnMut(AppEvent),
    id: GroupId,
    start: bool,
) {
    super::group_card(ui, resources, |card| {
        super::show_text(
            card,
            TypographyRole::Body,
            resources.tokens.ink_muted,
            resources.catalog.text(TextKey::GroupApplySelectionConflict),
        );
        card.horizontal_wrapped(|row| {
            if action(
                row,
                resources,
                TextKey::KeepSelectionDraft,
                !state.blocked(snapshot),
            ) {
                state.apply_choice = None;
            }
            if action(
                row,
                resources,
                TextKey::DiscardSelectionAndApply,
                !state.blocked(snapshot),
            ) {
                let command = GroupCommand::Apply { id, start };
                emit(AppEvent::DiscardStagedReceivers);
                state.submit(snapshot, command.clone());
                emit(AppEvent::GroupRequested(command));
                state.apply_choice = None;
            }
        });
    });
}

fn editor(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    groups: &[SavedGroupReading],
    state: &mut PageState,
    emit: &mut dyn FnMut(AppEvent),
) {
    let blocked = state.blocked(snapshot);
    let can_submit = state.can_submit(snapshot);
    let draft = state.draft.as_mut().expect("editor requires draft");
    let mut cancel = false;
    let mut sent = None;
    super::group_card(ui, resources, |card| {
        if blocked {
            card.disable();
        }
        super::show_text(
            card,
            TypographyRole::SectionTitle,
            resources.tokens.ink,
            resources.catalog.text(if draft.id.is_some() {
                TextKey::EditGroup
            } else {
                TextKey::CreateGroup
            }),
        );
        super::show_text(
            card,
            TypographyRole::Body,
            resources.tokens.ink,
            resources.catalog.text(TextKey::GroupName),
        );
        let field = card.add_sized(
            [card.available_width(), 40.0],
            egui::TextEdit::singleline(&mut draft.name)
                .desired_width(card.available_width())
                .char_limit(64),
        );
        let value = draft.name.clone();
        field.widget_info(|| {
            let mut info = WidgetInfo::text_edit(!blocked, &value, &value, "");
            info.label = Some(resources.catalog.text(TextKey::GroupName).to_owned());
            info
        });
        card.add_space(SECTION_GAP);
        super::show_text(
            card,
            TypographyRole::SectionTitle,
            resources.tokens.ink,
            resources.catalog.text(TextKey::GroupMembers),
        );
        members(card, snapshot, resources, draft);
        let validation = draft.validation().or_else(|| duplicate(draft, groups));
        if let Some(key) = validation.map(validation_key) {
            super::show_text(
                card,
                TypographyRole::Body,
                resources.tokens.fault,
                resources.catalog.text(key),
            );
        }
        card.horizontal_wrapped(|row| {
            if action(row, resources, TextKey::Cancel, !blocked) {
                cancel = true;
            }
            if action(
                row,
                resources,
                TextKey::SaveGroup,
                !blocked && can_submit && validation.is_none(),
            ) {
                let command = draft.command();
                draft.submitted = Some(command.clone());
                sent = Some(command.clone());
                emit(AppEvent::GroupRequested(command));
            }
        });
    });
    if cancel {
        state.draft = None;
    } else if let Some(command) = sent {
        state.submit(snapshot, command);
    }
}
fn members(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    draft: &mut GroupDraft,
) {
    let candidates = draft.candidates(snapshot);
    for (id, name, offline) in candidates {
        ui.push_id(("group_member", id.0), |ui| {
            ui.horizontal(|row| {
                row.spacing_mut().interact_size.y = 40.0;
                let mut chosen = draft.members.iter().any(|m| m.receiver == id);
                let label = if offline {
                    format!(
                        "{name} · {}",
                        resources.catalog.text(TextKey::GroupMemberOffline)
                    )
                } else {
                    name.clone()
                };
                row.allocate_ui_with_layout(
                    egui::vec2(220.0, 40.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |cell| {
                        cell.set_min_width(220.0);
                        if cell.checkbox(&mut chosen, label).changed() {
                            draft.set_member_selected(&id, &name, chosen);
                        }
                    },
                );
                if let Some(member) = draft.members.iter_mut().find(|m| m.receiver == id) {
                    let percent = (member.level * 100.0).round() as u8;
                    let model = crate::ui::presentation::AudioDockModel {
                        label: name.clone(),
                        accessible_name: name,
                        value: member.level,
                        percent,
                        percent_text: format!("{percent} %"),
                    };
                    row.spacing_mut().item_spacing.x = 0.0;
                    // Reuse the native track, but keep its value strictly local.
                    crate::ui::components::audio_dock::show_slider(
                        row,
                        resources.tokens,
                        &model,
                        &mut |event| {
                            if let AppEvent::MasterVolumeChanged(level) = event {
                                member.level = level;
                            }
                        },
                    );
                    row.add_space(crate::ui::components::CONTROL_GAP);
                    super::show_text(
                        row,
                        TypographyRole::Measurement,
                        resources.tokens.ink,
                        &model.percent_text,
                    );
                }
            });
        });
    }
}
fn duplicate(draft: &GroupDraft, groups: &[SavedGroupReading]) -> Option<GroupDraftValidation> {
    groups
        .iter()
        .any(|g| Some(g.id) != draft.id && g.name.eq_ignore_ascii_case(draft.name.trim()))
        .then_some(GroupDraftValidation::Duplicate)
}
fn validation_key(validation: GroupDraftValidation) -> TextKey {
    match validation {
        GroupDraftValidation::NameRequired => TextKey::GroupNameRequired,
        GroupDraftValidation::MemberRequired => TextKey::GroupMemberRequired,
        GroupDraftValidation::Duplicate => TextKey::GroupNameDuplicate,
    }
}
fn feedback(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    state: &mut PageState,
) {
    if let Some(failure) = state.admission_failure {
        super::show_text(
            ui,
            TypographyRole::Body,
            resources.tokens.fault,
            resources.catalog.text(match failure {
                GroupFailure::Closed => TextKey::GroupClosed,
                GroupFailure::ConfirmationLost => TextKey::GroupUnconfirmed,
                _ => TextKey::GroupBusy,
            }),
        );
        return;
    }
    if state.sent.is_some() && state.associated_request.is_none() {
        super::show_text(
            ui,
            TypographyRole::Body,
            resources.tokens.ink_muted,
            resources.catalog.text(TextKey::GroupOperationPending),
        );
        return;
    }
    let Some(op) = state
        .observed_operation
        .as_ref()
        .or(snapshot.group_operation.as_ref())
    else {
        return;
    };
    if state.acknowledged_request == Some(op.request)
        || state.associated_request != Some(op.request)
        || matches!(op.command, GroupCommand::ApplySelection { .. })
    {
        return;
    }
    let key = match op.status {
        GroupOperationStatus::Pending => TextKey::GroupOperationPending,
        GroupOperationStatus::Succeeded => match op.command {
            GroupCommand::Save { .. } => TextKey::GroupSaveSucceeded,
            GroupCommand::Delete(_) => TextKey::GroupDeleteSucceeded,
            GroupCommand::Apply { start: true, .. } => TextKey::GroupApplyAndStartSucceeded,
            GroupCommand::Apply { .. } => TextKey::GroupApplySucceeded,
            GroupCommand::ApplySelection { .. } => return,
        },
        GroupOperationStatus::Failed(GroupFailure::Busy) => TextKey::GroupBusy,
        GroupOperationStatus::Failed(GroupFailure::Validation) => TextKey::GroupValidationFailed,
        GroupOperationStatus::Failed(GroupFailure::Persistence) => TextKey::GroupPersistenceFailed,
        GroupOperationStatus::Failed(GroupFailure::Closed) => TextKey::GroupClosed,
        GroupOperationStatus::Failed(GroupFailure::ConfirmationLost) => TextKey::GroupUnconfirmed,
    };
    super::show_text(
        ui,
        TypographyRole::Body,
        resources.tokens.ink_muted,
        resources.catalog.text(key),
    );
    if op.status != GroupOperationStatus::Pending
        && action(ui, resources, TextKey::DismissGroupResult, true)
    {
        state.acknowledged_request = Some(op.request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_core::DeviceId;
    use std::collections::BTreeSet;
    #[test]
    fn group_draft_requires_a_name_and_a_member_before_it_can_be_saved() {
        let mut draft = GroupDraft::new();
        assert_eq!(draft.validation(), Some(GroupDraftValidation::NameRequired));
        draft.name = "Evening".into();
        assert_eq!(
            draft.validation(),
            Some(GroupDraftValidation::MemberRequired)
        );
        draft.members.push(GroupMember {
            receiver: DeviceId([0, 0, 0, 0, 0, 1]),
            name: "Living room".into(),
            level: 0.7,
        });
        assert_eq!(draft.validation(), None);
    }

    #[test]
    fn only_the_editor_request_can_settle_or_close_its_draft() {
        let command = GroupCommand::Save {
            id: None,
            name: "Evening".into(),
            members: vec![],
        };
        let mut state = PageState {
            draft: Some(GroupDraft::new()),
            ..PageState::default()
        };
        let mut app = crate::app::AppState::default();
        app.group_operation = Some(crate::app::GroupOperation {
            request: 4,
            command: command.clone(),
            status: GroupOperationStatus::Succeeded,
        });
        associate_outcome(&mut state, &UiSnapshot::from_state(&app));
        assert!(
            state.draft.is_some(),
            "an old result cannot close a new unsent editor"
        );
        state.sent = Some(command);
        state.draft.as_mut().unwrap().submitted = state.sent.clone();
        associate_outcome(&mut state, &UiSnapshot::from_state(&app));
        assert_eq!(state.associated_request, Some(4));
        assert!(
            state.draft.is_none(),
            "the correlated success closes the editor"
        );
    }

    #[test]
    fn failed_or_unrelated_operation_preserves_the_local_draft() {
        let command = GroupCommand::Save {
            id: None,
            name: "Evening".into(),
            members: vec![],
        };
        let mut state = PageState {
            draft: Some(GroupDraft::new()),
            sent: Some(command),
            ..PageState::default()
        };
        let mut app = crate::app::AppState::default();
        app.group_operation = Some(crate::app::GroupOperation {
            request: 5,
            command: GroupCommand::ApplySelection {
                receiver_ids: vec![],
            },
            status: GroupOperationStatus::Failed(GroupFailure::Persistence),
        });
        associate_outcome(&mut state, &UiSnapshot::from_state(&app));
        assert!(state.draft.is_some());
        assert_eq!(state.associated_request, None);
    }

    #[test]
    fn apply_and_apply_and_start_are_distinct_and_dirty_selection_needs_a_choice() {
        let id = GroupId(uuid::Uuid::nil());
        let mut app = crate::app::AppState::default();
        let receiver = DeviceId([0, 0, 0, 0, 0, 1]);
        app.staged_receivers.insert(receiver.clone());
        let snapshot = UiSnapshot::from_state(&app);
        let mut state = PageState::default();
        let mut events = Vec::new();
        request_apply(&snapshot, &mut state, id, false, &mut |event| {
            events.push(event)
        });
        assert_eq!(state.apply_choice, Some((id, false)));
        assert!(
            events.is_empty(),
            "the dirty selection is never silently lost"
        );
        app.desired_receivers.insert(receiver);
        let snapshot = UiSnapshot::from_state(&app);
        request_apply(&snapshot, &mut state, id, true, &mut |event| {
            events.push(event)
        });
        assert_eq!(
            events,
            vec![AppEvent::GroupRequested(GroupCommand::Apply {
                id,
                start: true
            })]
        );
    }

    #[test]
    fn edit_copies_offline_member_data() {
        let offline = GroupMember {
            receiver: DeviceId([0, 0, 0, 0, 0, 9]),
            name: "Office".into(),
            level: 0.35,
        };
        let group = SavedGroupReading {
            id: GroupId(uuid::Uuid::nil()),
            name: "Evening".into(),
            members: vec![offline.clone()],
            available_members: BTreeSet::new(),
        };
        let draft = GroupDraft::from_group(&group);
        assert_eq!(draft.members, vec![offline]);
    }

    #[test]
    fn offline_member_can_be_reselected_next_frame_with_its_last_level() {
        let id = DeviceId([9; 6]);
        let group = SavedGroupReading {
            id: GroupId(uuid::Uuid::nil()),
            name: "Evening".into(),
            members: vec![GroupMember {
                receiver: id.clone(),
                name: "Office".into(),
                level: 0.35,
            }],
            available_members: BTreeSet::new(),
        };
        let mut draft = GroupDraft::from_group(&group);
        let snapshot = UiSnapshot::from_state(&crate::app::AppState::default());
        draft.members[0].level = 0.45;
        for _ in 0..3 {
            draft.set_member_selected(&id, "Office", false);
            assert!(draft.members.is_empty());
            assert_eq!(
                draft.candidates(&snapshot),
                vec![(id.clone(), "Office".into(), true)]
            );
            draft.set_member_selected(&id, "Office", true);
            assert_eq!(draft.members.len(), 1);
            assert_eq!(draft.members[0].level, 0.45);
        }
    }

    #[test]
    fn delete_confirmation_names_the_exact_group() {
        let catalog = crate::ui::i18n::Catalog::new(crate::app::ResolvedLocale::English);
        assert_eq!(
            catalog.group_delete_confirmation("Evening"),
            "Delete group \"Evening\"?"
        );
    }
}
