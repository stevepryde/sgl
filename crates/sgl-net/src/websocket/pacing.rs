//! Browser send pacing against `bufferedAmount`, kept free of `web_sys` so
//! it runs in native tests.

use crate::Delivery;

/// What a browser flush does with the next released frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pace {
    Send,
    /// Leave the due latest state coalesced and send lane frames instead.
    SkipLatest,
    /// Stop until the browser drains.
    Wait,
}

/// Paces released frames against the browser's `bufferedAmount`, which the
/// browser sends in order. Lane frames wait for the reliable watermark. A
/// due latest state leaves under the latest watermark, or once every byte
/// buffered ahead of it when it first waited has drained; lane frames keep
/// flowing meanwhile, so it waits only for the frames flushed with or
/// before it (netcode.md 13). Every frame stays under the reliable
/// watermark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct BufferedPacing {
    /// Bytes sent since a due latest state first waited; `None` while none
    /// waits. A newer state that supersedes it keeps its place.
    sent_since_latest_waited: Option<usize>,
}

impl BufferedPacing {
    pub(super) fn next(
        &mut self,
        buffered: usize,
        delivery: Delivery,
        frame_bytes: usize,
        reliable_buffered_bytes: usize,
        latest_buffered_bytes: usize,
    ) -> Pace {
        let fits = |limit: usize| buffered.saturating_add(frame_bytes) <= limit;
        match delivery {
            Delivery::LatestState => {
                // What remains buffered is no more than what was sent after
                // the state began waiting, so everything ahead of it has gone.
                let ahead_drained = self
                    .sent_since_latest_waited
                    .is_some_and(|sent| buffered <= sent);
                if fits(reliable_buffered_bytes) && (fits(latest_buffered_bytes) || ahead_drained) {
                    Pace::Send
                } else {
                    self.sent_since_latest_waited.get_or_insert(0);
                    Pace::SkipLatest
                }
            }
            Delivery::Reliable(_) | Delivery::Unreliable(_) => {
                if fits(reliable_buffered_bytes) {
                    Pace::Send
                } else {
                    Pace::Wait
                }
            }
        }
    }

    pub(super) fn sent(&mut self, delivery: Delivery, frame_bytes: usize) {
        if delivery == Delivery::LatestState {
            self.sent_since_latest_waited = None;
        } else if let Some(sent) = &mut self.sent_since_latest_waited {
            *sent = sent.saturating_add(frame_bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Defect: a due latest state over its watermark either held every lane
    /// behind it, or let saturated lanes keep `bufferedAmount` above that
    /// watermark for as long as their backlog lasted. Oracle: netcode.md 13
    /// — the state waits only for what was flushed before it, and is not a
    /// barrier. A socket that drains 32 KiB a tick holds 200 KiB when the
    /// state falls due, and the lanes always have a 16 KiB frame ready: lane
    /// frames go out every tick while the state waits, and the state leaves
    /// at the first flush after the socket has drained those 200 KiB.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_waiting_latest_state_leaves_once_the_bytes_ahead_of_it_drain() {
        const KIB: usize = 1024;
        const RELIABLE: usize = 256 * KIB;
        const LATEST: usize = 64 * KIB;
        const LANE_FRAME: usize = 16 * KIB;
        const STATE_FRAME: usize = KIB;
        const AHEAD: usize = 200 * KIB;
        const DRAIN_PER_TICK: usize = 32 * KIB;

        let mut pacing = BufferedPacing::default();
        let mut socket: VecDeque<usize> = std::iter::repeat_n(LANE_FRAME, AHEAD / LANE_FRAME)
            .chain([AHEAD % LANE_FRAME])
            .collect();
        let mut drained = 0;
        let mut state_left = None;
        let mut ahead_drained_at = None;
        for tick in 0..32 {
            let mut budget = if tick == 0 { 0 } else { DRAIN_PER_TICK };
            while budget > 0
                && let Some(front) = socket.front_mut()
            {
                let taken = budget.min(*front);
                *front -= taken;
                budget -= taken;
                drained += taken;
                if *front == 0 {
                    socket.pop_front();
                }
            }
            if drained >= AHEAD {
                ahead_drained_at.get_or_insert(tick);
            }

            let mut lane_frames_sent = 0;
            let mut latest_waits = state_left.is_some();
            loop {
                let buffered: usize = socket.iter().sum();
                let (delivery, frame_bytes) = if latest_waits {
                    (Delivery::RELIABLE_ORDERED, LANE_FRAME)
                } else {
                    (Delivery::LatestState, STATE_FRAME)
                };
                match pacing.next(buffered, delivery, frame_bytes, RELIABLE, LATEST) {
                    Pace::Send => {
                        socket.push_back(frame_bytes);
                        pacing.sent(delivery, frame_bytes);
                        if delivery == Delivery::LatestState {
                            state_left = Some(tick);
                            latest_waits = true;
                        } else {
                            lane_frames_sent += 1;
                        }
                    }
                    Pace::SkipLatest => latest_waits = true,
                    Pace::Wait => break,
                }
            }
            assert!(lane_frames_sent > 0, "lanes held at tick {tick}");
            if state_left.is_some() {
                break;
            }
        }
        assert!(
            ahead_drained_at.is_some(),
            "the scenario drains what was ahead"
        );
        assert_eq!(state_left, ahead_drained_at);
    }
}
