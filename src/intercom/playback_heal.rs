//! Issue 1539: the I/O half of the intercom playback self-heal (the decisions live in the pure
//! `crate::playback_stall`). Cheap reads on the intercom thread only, never on the capture/NDI
//! path: the headset card identity from `/proc/asound`, and the playback-only re-open under the
//! [`ReopenBudget`].

use alsa::pcm::PCM;
use anyhow::Result;

use crate::playback_stall::{
    card_index_from_link, CardIdentity, IdentityRead, ReopenBudget, StallReason, REOPEN_BUDGET_MAX,
    REOPEN_BUDGET_WINDOW_NS,
};

const HID_CARD_LINK: &str = "/proc/asound/HID";

fn read_usbbus(card: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/asound/card{card}/usbbus"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The identity of the card the playback handle was opened on (`None` = unreadable).
pub(super) fn opened_card_identity(pcm: &PCM) -> Option<CardIdentity> {
    let card = pcm.info().ok()?.get_card();
    Some(CardIdentity {
        card,
        usbbus: read_usbbus(card)?,
    })
}

/// The identity of the card named `HID` right now (`/proc/asound/HID` -> `cardN`).
pub(super) fn current_hid_identity() -> IdentityRead {
    let target = match std::fs::read_link(HID_CARD_LINK) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return IdentityRead::Gone,
        Err(_) => return IdentityRead::Unknown,
    };
    let Some(card) = target.to_str().and_then(card_index_from_link) else {
        return IdentityRead::Unknown;
    };
    match read_usbbus(card) {
        Some(usbbus) => IdentityRead::Present(CardIdentity { card, usbbus }),
        None => IdentityRead::Unknown,
    }
}

pub(super) fn stall_reason_text(reason: &StallReason) -> String {
    match reason {
        StallReason::NotConsuming { consumed, expected } => format!(
            "the device consumed {consumed} of {expected} frames in a window with a full ring while VBAN audio arrives"
        ),
        StallReason::CardReenumerated { opened, now } => format!(
            "the headset card was re-enumerated (opened card{} usbbus {}, now card{} usbbus {})",
            opened.card, opened.usbbus, now.card, now.usbbus
        ),
        StallReason::CardGone => "the headset card is gone".to_string(),
    }
}

pub(super) enum Reopen {
    /// The fresh handle + the card identity it was opened on.
    Done(PCM, Option<CardIdentity>),
    /// The budget is used up: the OLD handle comes back untouched.
    Refused(PCM),
}

/// Per-process playback self-heal state: survives the intercom's inner restarts.
pub(super) struct PlaybackHeal {
    reopens: u64,
    budget: ReopenBudget,
    epoch: std::time::Instant,
    /// Re-opens refused since the last granted one (logged once per run of refusals).
    refused: u64,
    /// Every refused re-open since this process started (the status line's `playback_refused`).
    refused_total: u64,
}

impl PlaybackHeal {
    pub(super) fn new() -> Self {
        Self {
            reopens: 0,
            budget: ReopenBudget::new(REOPEN_BUDGET_MAX, REOPEN_BUDGET_WINDOW_NS),
            epoch: std::time::Instant::now(),
            refused: 0,
            refused_total: 0,
        }
    }

    /// Playback re-opens since this process started (the status line's `playback_reopens`).
    pub(super) fn reopens(&self) -> u64 {
        self.reopens
    }

    /// Re-opens the budget refused since this process started (`playback_refused`).
    pub(super) fn refused_total(&self) -> u64 {
        self.refused_total
    }

    /// Close + re-open ONLY the playback PCM with `open` (the unchanged `open_alsa_playback`:
    /// same period and buffer), logging the reason once per event. Capture and the VBAN
    /// receiver keep running. `Err` = the open failed (the caller restarts the whole intercom).
    pub(super) fn reopen(
        &mut self,
        old: PCM,
        reason: &str,
        open: impl FnOnce() -> Result<PCM>,
    ) -> Result<Reopen> {
        let now_ns = self.epoch.elapsed().as_nanos() as u64;
        if !self.budget.try_spend(now_ns) {
            if self.refused == 0 {
                tracing::warn!(
                    "#1539 playback self-heal: {} re-opens in {} s used up, not re-opening ({}); further refusals stay quiet until a re-open is granted",
                    REOPEN_BUDGET_MAX,
                    REOPEN_BUDGET_WINDOW_NS / 1_000_000_000,
                    reason
                );
            }
            self.refused += 1;
            self.refused_total += 1;
            return Ok(Reopen::Refused(old));
        }
        self.reopens += 1;
        tracing::warn!(
            "#1539 playback self-heal: re-opening hw:CARD=HID playback ({}), reopen #{}{}",
            reason,
            self.reopens,
            if self.refused > 0 {
                format!(", {} refused before", self.refused)
            } else {
                String::new()
            }
        );
        self.refused = 0;
        // hw: devices are exclusive: the old handle closes (snd_pcm_close on drop) before the open.
        drop(old);
        let fresh = open()?;
        let identity = opened_card_identity(&fresh);
        Ok(Reopen::Done(fresh, identity))
    }
}
