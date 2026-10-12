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
                } else if ahead_drained {
                    // Its turn has come: lanes refilling the room it waits
                    // for would starve it on a slow link.
                    Pace::Wait
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

    /// A socket that holds `ahead` bytes when a latest state of
    /// `state_frame` bytes falls due, drains `drain_per_tick` bytes a tick,
    /// and has a lane frame of `lane_frame` bytes always ready, under
    /// 256 KiB reliable and 64 KiB latest watermarks.
    struct SlowSocket {
        ahead: usize,
        drain_per_tick: usize,
        lane_frame: usize,
        state_frame: usize,
    }

    /// What happened, by tick.
    struct Run {
        /// The tick the socket had drained every byte ahead of the state.
        ahead_drained_at: Option<usize>,
        /// The tick the state was sent.
        state_left: Option<usize>,
        /// Lane frames sent each tick.
        lane_frames: Vec<usize>,
    }

    impl SlowSocket {
        fn run(&self, ticks: usize) -> Run {
            const RELIABLE: usize = 256 * 1024;
            const LATEST: usize = 64 * 1024;
            let mut pacing = BufferedPacing::default();
            let mut socket = VecDeque::from([self.ahead]);
            let mut drained = 0;
            let mut run = Run {
                ahead_drained_at: None,
                state_left: None,
                lane_frames: Vec::new(),
            };
            for tick in 0..ticks {
                let mut budget = if tick == 0 { 0 } else { self.drain_per_tick };
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
                if drained >= self.ahead {
                    run.ahead_drained_at.get_or_insert(tick);
                }
                let mut lane_frames = 0;
                let mut latest_waits = run.state_left.is_some();
                loop {
                    let buffered: usize = socket.iter().sum();
                    let (delivery, frame_bytes) = if latest_waits {
                        (Delivery::RELIABLE_ORDERED, self.lane_frame)
                    } else {
                        (Delivery::LatestState, self.state_frame)
                    };
                    match pacing.next(buffered, delivery, frame_bytes, RELIABLE, LATEST) {
                        Pace::Send => {
                            socket.push_back(frame_bytes);
                            pacing.sent(delivery, frame_bytes);
                            if delivery == Delivery::LatestState {
                                run.state_left = Some(tick);
                                latest_waits = true;
                            } else {
                                lane_frames += 1;
                            }
                        }
                        Pace::SkipLatest => latest_waits = true,
                        Pace::Wait => break,
                    }
                }
                run.lane_frames.push(lane_frames);
                if run.state_left.is_some() {
                    break;
                }
            }
            run
        }
    }

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
        let run = SlowSocket {
            ahead: 200 * 1024,
            drain_per_tick: 32 * 1024,
            lane_frame: 16 * 1024,
            state_frame: 1024,
        }
        .run(32);
        assert!(
            run.ahead_drained_at.is_some(),
            "the scenario drains what was ahead"
        );
        assert_eq!(run.state_left, run.ahead_drained_at);
        assert!(
            run.lane_frames.iter().all(|&frames| frames > 0),
            "lanes held: {:?}",
            run.lane_frames
        );
    }

    /// Defect: on a link slower than one state frame per tick, lanes that
    /// keep going after the bytes ahead of the state have drained take every
    /// bit of room it waits for, so it never leaves. Oracle: netcode.md 13 —
    /// once those bytes have gone the state leaves before any further lane
    /// frame, so it waits at most for room for itself.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_latest_state_whose_turn_has_come_is_not_starved_on_a_slow_link() {
        let socket = SlowSocket {
            ahead: 250 * 1024,
            drain_per_tick: 256,
            lane_frame: 512,
            state_frame: 8 * 1024,
        };
        let run = socket.run(4_000);
        let ahead_drained_at = run
            .ahead_drained_at
            .expect("the scenario drains what was ahead");
        let left = run.state_left.expect("the state never left");
        assert!(
            left <= ahead_drained_at + socket.state_frame / socket.drain_per_tick + 1,
            "left at tick {left}, {ahead_drained_at} drained what was ahead"
        );
    }
}
