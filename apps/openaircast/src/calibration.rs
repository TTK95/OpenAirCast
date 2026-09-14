//! Manual per-receiver presentation-time calibration domain.
//!
//! The user authors a signed relative delay per receiver; [`normalize_calibration`]
//! converts that intent into the non-negative effective delays applied at
//! session setup: every requested value is shifted so the minimum becomes
//! zero, meaning the earliest speaker keeps its timing while all other
//! speakers are delayed relative to it. Pairwise differences are preserved.
//! Normalization considers only the receivers in the active selection;
//! profile entries for receivers outside the selection are ignored so stale
//! membership can never shift the group.
//!
//! Calibration is restart-only and persisted by stable [`ReceiverId`] through
//! `backend::persistence`; it never enters shell settings. [`CalibrationCommand`]
//! travels to the backend actor as `BackendCommand::Calibration(..)`: an
//! accepted apply or reset is written to disk first and then costs exactly one
//! controlled full-group restart, because the normalized delays are handed to
//! the targets while the group is built and cannot be changed mid-stream.
//!
//! [`effective_delays_by_device`] is the single crossing between the backend's
//! [`ReceiverId`] and the AirPlay client's `DeviceId`; there is deliberately
//! no second one.

use std::collections::{BTreeMap, BTreeSet};

use airplay_core::DeviceId;

use crate::backend::model::ReceiverId;
use crate::backend::persistence::CalibrationStateV2;

/// UI display step for signed relative delay adjustments: 0.1 ms.
pub const CALIBRATION_DISPLAY_STEP_NS: i64 = 100_000;

/// Fixed conservative digital amplitude of each click in the shared
/// four-click alignment test.
pub const CALIBRATION_CLICK_AMPLITUDE: i16 = 4_096;

/// Number of clicks in the shared alignment pattern.
pub const CALIBRATION_CLICK_COUNT: u32 = 4;

/// Width of one click in milliseconds.
pub const CALIBRATION_CLICK_WIDTH_MS: u32 = 2;

/// Distance between two click onsets in milliseconds.
pub const CALIBRATION_CLICK_SPACING_MS: u32 = 500;

/// User-authored calibration intent keyed by stable [`ReceiverId`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalibrationProfile {
    /// Reference receiver the relative delays are judged against; must be
    /// part of the active selection when set.
    pub reference_receiver: Option<ReceiverId>,
    /// Signed requested relative delay per receiver in nanoseconds.
    /// Active receivers absent from this map request zero.
    pub requested_relative_delay_ns: BTreeMap<ReceiverId, i64>,
}

/// Normalized, non-negative delays ready for controlled session setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveCalibration {
    /// Smallest requested delay in the active selection; shifted to zero.
    pub minimum_requested_ns: i64,
    /// Effective added presentation delay per active receiver in nanoseconds.
    pub effective_delay_ns: BTreeMap<ReceiverId, u64>,
}

/// Errors raised while normalizing a profile against an active selection.
#[derive(Debug, thiserror::Error)]
pub enum CalibrationError {
    /// Calibration requires at least one active receiver.
    #[error("calibration requires a non-empty active receiver selection")]
    EmptySelection,
    /// The chosen reference receiver is not part of the active selection.
    #[error("the reference receiver is not part of the active selection")]
    ReferenceNotInSelection,
    /// Checked arithmetic rejected the normalized delay mapping.
    #[error("normalized calibration delays do not fit the target integer range")]
    ArithmeticOverflow,
}

/// Calibration control commands carried by `BackendCommand::Calibration`.
///
/// Each apply/reset is guarded by the desired revision the caller decided
/// against, persisted before it is applied, and worth exactly one controlled
/// full-group restart. The click test changes no setup input and therefore
/// costs no restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalibrationCommand {
    /// Validate, persist, and apply `profile`, guarded against lost updates
    /// by the expected desired revision.
    ApplyCalibrationProfile {
        /// Desired revision the caller based its decision on.
        expected_desired_revision: u64,
        /// The profile to persist and apply.
        profile: CalibrationProfile,
    },
    /// Clear every relative delay through the same controlled restart path.
    ResetCalibration {
        /// Desired revision the caller based its decision on.
        expected_desired_revision: u64,
    },
    /// Inject the shared four-click PCM pattern into the live capture bridge
    /// so it traverses the normal encode/RTP fan-out path.
    RunCalibrationClickTest,
}

impl From<&CalibrationProfile> for CalibrationStateV2 {
    fn from(profile: &CalibrationProfile) -> Self {
        Self {
            reference: profile.reference_receiver,
            delays_ns: profile.requested_relative_delay_ns.clone(),
        }
    }
}

impl From<&CalibrationStateV2> for CalibrationProfile {
    fn from(section: &CalibrationStateV2) -> Self {
        Self {
            reference_receiver: section.reference,
            requested_relative_delay_ns: section.delays_ns.clone(),
        }
    }
}

/// Re-keys normalized delays from the backend's [`ReceiverId`] onto the
/// AirPlay client's [`DeviceId`].
///
/// This is the *only* place the two identity spaces meet. Both wrap the same
/// six MAC bytes, so the conversion is lossless in either direction and the
/// map keeps its cardinality: every active receiver contributes exactly one
/// entry, and nothing is invented for a receiver that is not in it. The
/// client adapter downstream rejects a map that does not match its connected
/// targets one-for-one rather than defaulting a missing entry to zero -- a
/// silent zero would move that speaker relative to the rest without anyone
/// having asked for it.
pub fn effective_delays_by_device(effective: &EffectiveCalibration) -> BTreeMap<DeviceId, u64> {
    effective
        .effective_delay_ns
        .iter()
        .map(|(receiver, delay)| (DeviceId::from(*receiver), *delay))
        .collect()
}

/// Number of whole frames `milliseconds` occupies at `sample_rate`.
fn frames_for_ms(sample_rate: u32, milliseconds: u32) -> usize {
    (u64::from(sample_rate) * u64::from(milliseconds) / 1_000) as usize
}

/// Builds the shared four-click alignment pattern as interleaved PCM.
///
/// Four clicks of [`CALIBRATION_CLICK_WIDTH_MS`] at the fixed amplitude
/// [`CALIBRATION_CLICK_AMPLITUDE`], their onsets
/// [`CALIBRATION_CLICK_SPACING_MS`] apart, silence everywhere else. The
/// pattern carries nothing receiver-specific: it is injected once into the
/// shared PCM bridge and reaches every receiver through the normal
/// ALAC/RTP fan-out, so what a listener hears is only the per-target
/// presentation clock and never a different signal per speaker.
pub fn calibration_click_pattern(sample_rate: u32, channels: u8) -> Vec<i16> {
    let channels = usize::from(channels.max(1));
    let click_frames = frames_for_ms(sample_rate, CALIBRATION_CLICK_WIDTH_MS);
    let spacing_frames = frames_for_ms(sample_rate, CALIBRATION_CLICK_SPACING_MS);
    let clicks = CALIBRATION_CLICK_COUNT as usize;
    let total_frames = spacing_frames * clicks.saturating_sub(1) + click_frames;

    let mut samples = vec![0_i16; total_frames * channels];
    for click in 0..clicks {
        let start = click * spacing_frames;
        for frame in start..start + click_frames {
            let base = frame * channels;
            for slot in &mut samples[base..base + channels] {
                *slot = CALIBRATION_CLICK_AMPLITUDE;
            }
        }
    }
    samples
}

/// Requests the signed relative delay recorded for `receiver`.
///
/// Active receivers missing from the profile request zero nanoseconds.
fn requested_delay_ns(profile: &CalibrationProfile, receiver: &ReceiverId) -> i64 {
    profile
        .requested_relative_delay_ns
        .get(receiver)
        .copied()
        .unwrap_or(0)
}

/// Normalizes `profile` for exactly the receivers in `active`.
///
/// Semantics:
///
/// - An empty `active` selection is rejected with
///   [`CalibrationError::EmptySelection`].
/// - When set, `profile.reference_receiver` must belong to `active`, otherwise
///   [`CalibrationError::ReferenceNotInSelection`] is returned.
/// - Only active receivers participate: entries in the profile for receivers
///   outside `active` are **ignored** (not an error), and active receivers
///   missing from the map are treated as requesting zero.
/// - All requested values shift by `-minimum_requested_ns`, so the earliest
///   speaker is unchanged and every other speaker gains the same relative
///   delay as before; results are therefore always non-negative.
/// - Subtraction happens in `i128` and narrows through `u64::try_from`, so an
///   out-of-range result surfaces as
///   [`CalibrationError::ArithmeticOverflow`] instead of wrapping.
pub fn normalize_calibration(
    active: &BTreeSet<ReceiverId>,
    profile: &CalibrationProfile,
) -> Result<EffectiveCalibration, CalibrationError> {
    if active.is_empty() {
        return Err(CalibrationError::EmptySelection);
    }
    if let Some(reference) = profile.reference_receiver {
        if !active.contains(&reference) {
            return Err(CalibrationError::ReferenceNotInSelection);
        }
    }

    let minimum_requested_ns = active
        .iter()
        .map(|receiver| requested_delay_ns(profile, receiver))
        .min()
        .unwrap_or_default();

    let mut effective_delay_ns = BTreeMap::new();
    for receiver in active {
        let shifted =
            i128::from(requested_delay_ns(profile, receiver)) - i128::from(minimum_requested_ns);
        let shifted = u64::try_from(shifted).map_err(|_| CalibrationError::ArithmeticOverflow)?;
        effective_delay_ns.insert(*receiver, shifted);
    }

    Ok(EffectiveCalibration {
        minimum_requested_ns,
        effective_delay_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    mod normalize {
        use super::*;

        #[test]
        fn single_receiver_normalizes_to_zero() {
            let active = BTreeSet::from([receiver(1)]);
            let profile = CalibrationProfile {
                reference_receiver: Some(receiver(1)),
                requested_relative_delay_ns: BTreeMap::from([(receiver(1), -7)]),
            };

            let effective = normalize_calibration(&active, &profile).unwrap();

            assert_eq!(effective.minimum_requested_ns, -7);
            assert_eq!(effective.effective_delay_ns[&receiver(1)], 0);
        }

        fn receiver(seed: u8) -> ReceiverId {
            ReceiverId::from_storage_key(&format!("0000000000{seed:02X}")).unwrap()
        }
    }
}
