//! Player chunk-view geometry and ordered, resumable tracking changes.
//!
//! Requirements were inspected in locked 26.3-pre-2 ChunkTrackingView and
//! ChunkMap, then implemented independently. This owner has no heap allocations,
//! tickets, simulation-distance policy, chunk readiness, or network writer.

use std::{fmt, iter::FusedIterator};

use super::preparation::ChunkAddress;

pub const MIN_VIEW_DISTANCE: u8 = 2;
pub const MAX_VIEW_DISTANCE: u8 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewDistance(u8);

impl ViewDistance {
    /// Server configuration is clamped before evaluating any player request.
    pub fn server(requested: i32) -> Self {
        Self(requested.clamp(i32::from(MIN_VIEW_DISTANCE), i32::from(MAX_VIEW_DISTANCE)) as u8)
    }

    /// The decoded client-information byte is signed; widen its i8 value to i32.
    pub fn effective(self, client_requested: i32) -> Self {
        Self(client_requested.clamp(i32::from(MIN_VIEW_DISTANCE), i32::from(self.0)) as u8)
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewError {
    CoordinateBounds,
    UpdateActive,
    NoUpdate,
    NoEvent,
    UnconsumedEvents,
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "player chunk view: {self:?}")
    }
}
impl std::error::Error for ViewError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Bounds {
    min: ChunkAddress,
    max: ChunkAddress,
}

impl Bounds {
    fn intersects(self, other: Self) -> bool {
        self.min.x <= other.max.x
            && other.min.x <= self.max.x
            && self.min.z <= other.max.z
            && other.min.z <= self.max.z
    }

    fn union(self, other: Self) -> Self {
        Self {
            min: ChunkAddress {
                x: self.min.x.min(other.min.x),
                z: self.min.z.min(other.min.z),
            },
            max: ChunkAddress {
                x: self.max.x.max(other.max.x),
                z: self.max.z.max(other.max.z),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PositionedView {
    center: ChunkAddress,
    distance: ViewDistance,
    bounds: Bounds,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackingView(Option<PositionedView>);

impl TrackingView {
    pub const EMPTY: Self = Self(None);

    /// Rejects centers whose Java scan bounds or final loop increment overflow.
    /// This explicit admission policy avoids unbounded iteration at i32 edges;
    /// ordinary Minecraft world coordinates fit comfortably inside this range.
    pub fn positioned(center: ChunkAddress, distance: ViewDistance) -> Result<Self, ViewError> {
        let extent = i32::from(distance.get()) + 1;
        let minimum = |value: i32| value.checked_sub(extent).ok_or(ViewError::CoordinateBounds);
        let maximum = |value: i32| {
            value
                .checked_add(extent)
                .filter(|&max| max != i32::MAX)
                .ok_or(ViewError::CoordinateBounds)
        };
        Ok(Self(Some(PositionedView {
            center,
            distance,
            bounds: Bounds {
                min: ChunkAddress {
                    x: minimum(center.x)?,
                    z: minimum(center.z)?,
                },
                max: ChunkAddress {
                    x: maximum(center.x)?,
                    z: maximum(center.z)?,
                },
            },
        })))
    }

    pub fn center(self) -> Option<ChunkAddress> {
        self.0.map(|view| view.center)
    }

    pub fn distance(self) -> Option<ViewDistance> {
        self.0.map(|view| view.distance)
    }

    /// Tracking includes the neighboring chunk border used by ChunkTrackingView.
    pub fn contains(self, chunk: ChunkAddress) -> bool {
        self.0
            .is_some_and(|view| is_within_distance(view.center, view.distance, chunk, true))
    }

    pub fn is_in_view_distance(self, chunk: ChunkAddress) -> bool {
        self.0
            .is_some_and(|view| is_within_distance(view.center, view.distance, chunk, false))
    }
}

/// Exact Java i32 subtraction/absolute-value behavior, including MIN_VALUE.
/// This predicate accepts arbitrary coordinates without attempting to scan them.
pub fn is_within_distance(
    center: ChunkAddress,
    distance: ViewDistance,
    chunk: ChunkAddress,
    include_neighbors: bool,
) -> bool {
    let border = if include_neighbors { 2 } else { 1 };
    let axis = |value: i32, origin: i32| {
        i64::from(
            value
                .wrapping_sub(origin)
                .wrapping_abs()
                .wrapping_sub(border)
                .max(0),
        )
    };
    let x = axis(chunk.x, center.x);
    let z = axis(chunk.z, center.z);
    let radius = i64::from(distance.get());
    x * x + z * z < radius * radius
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewChange {
    Enter(ChunkAddress),
    Leave(ChunkAddress),
}

struct Scan {
    bounds: Bounds,
    next: Option<ChunkAddress>,
}

impl Scan {
    fn new(bounds: Bounds) -> Self {
        Self {
            bounds,
            next: Some(bounds.min),
        }
    }

    fn next(&mut self) -> Option<ChunkAddress> {
        let result = self.next?;
        self.next = if result.z < self.bounds.max.z {
            Some(ChunkAddress {
                x: result.x,
                z: result.z + 1,
            })
        } else if result.x < self.bounds.max.x {
            Some(ChunkAddress {
                x: result.x + 1,
                z: self.bounds.min.z,
            })
        } else {
            None
        };
        Some(result)
    }
}

#[derive(Clone, Copy)]
enum DifferencePhase {
    Overlap,
    Leave,
    Enter,
    Done,
}

/// Streams exact effect order with constant storage. An overlapping union visits
/// at most 133*133 cells; a disjoint teleport visits only the two 67*67 squares.
pub struct ViewDifference {
    previous: TrackingView,
    next: TrackingView,
    scan: Option<Scan>,
    phase: DifferencePhase,
    #[cfg(test)]
    visited: usize,
}

impl ViewDifference {
    pub fn new(previous: TrackingView, next: TrackingView) -> Self {
        let (phase, bounds) = if previous == next {
            (DifferencePhase::Done, None)
        } else if let (Some(old), Some(new)) = (previous.0, next.0)
            && old.bounds.intersects(new.bounds)
        {
            (DifferencePhase::Overlap, Some(old.bounds.union(new.bounds)))
        } else {
            (DifferencePhase::Leave, previous.0.map(|view| view.bounds))
        };
        Self {
            previous,
            next,
            scan: bounds.map(Scan::new),
            phase,
            #[cfg(test)]
            visited: 0,
        }
    }
}

impl Iterator for ViewDifference {
    type Item = ViewChange;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.scan.as_mut().and_then(Scan::next) {
                #[cfg(test)]
                {
                    self.visited += 1;
                }
                let was =
                    !matches!(self.phase, DifferencePhase::Enter) && self.previous.contains(chunk);
                let now =
                    !matches!(self.phase, DifferencePhase::Leave) && self.next.contains(chunk);
                if was != now {
                    return Some(if now {
                        ViewChange::Enter(chunk)
                    } else {
                        ViewChange::Leave(chunk)
                    });
                }
            } else if matches!(self.phase, DifferencePhase::Leave) {
                self.phase = DifferencePhase::Enter;
                self.scan = self.next.0.map(|view| Scan::new(view.bounds));
            } else {
                self.phase = DifferencePhase::Done;
                self.scan = None;
                return None;
            }
        }
    }
}

impl FusedIterator for ViewDifference {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewEvent {
    SetCenter(ChunkAddress),
    Enter(ChunkAddress),
    Leave(ChunkAddress),
}

impl From<ViewChange> for ViewEvent {
    fn from(change: ViewChange) -> Self {
        match change {
            ViewChange::Enter(chunk) => Self::Enter(chunk),
            ViewChange::Leave(chunk) => Self::Leave(chunk),
        }
    }
}

struct Transition {
    next: TrackingView,
    changes: ViewDifference,
    event: Option<ViewEvent>,
}

/// One player/level's tracking owner. Drain a transition before requesting
/// another: cancellation/rebasing cannot undo effects already applied outside.
/// Backpressure deliberately suspends the operation with its old current view;
/// the caller must defer dependent world/readiness callbacks until it finishes.
/// This resumable admission policy is not Vanilla intermediate-state parity.
pub struct PlayerView {
    current: TrackingView,
    transition: Option<Transition>,
}

impl Default for PlayerView {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerView {
    pub fn new() -> Self {
        Self {
            current: TrackingView::EMPTY,
            transition: None,
        }
    }

    pub fn current(&self) -> TrackingView {
        self.current
    }

    pub fn begin_update(&mut self, next: TrackingView) -> Result<(), ViewError> {
        if self.transition.is_some() {
            return Err(ViewError::UpdateActive);
        }
        let mut changes = ViewDifference::new(self.current, next);
        let event = match next.center() {
            Some(center) if Some(center) != self.current.center() => {
                Some(ViewEvent::SetCenter(center))
            }
            _ => changes.next().map(ViewEvent::from),
        };
        self.transition = Some(Transition {
            next,
            changes,
            event,
        });
        Ok(())
    }

    /// Repeated reads return the same event until acknowledgement. Enter means
    /// tracking entered: only mark a chunk pending when the authoritative world
    /// says it is send-ready. A future readiness callback must recheck the view.
    pub fn pending_event(&self) -> Option<ViewEvent> {
        self.transition
            .as_ref()
            .and_then(|transition| transition.event)
    }

    /// Call only after the event's external effect was successfully admitted.
    /// A full sender/delivery queue leaves the event pending for an exact retry.
    pub fn acknowledge_event(&mut self) -> Result<(), ViewError> {
        let transition = self.transition.as_mut().ok_or(ViewError::NoUpdate)?;
        if transition.event.is_none() {
            return Err(ViewError::NoEvent);
        }
        transition.event = transition.changes.next().map(ViewEvent::from);
        Ok(())
    }

    pub fn finish_update(&mut self) -> Result<(), ViewError> {
        let transition = self.transition.as_ref().ok_or(ViewError::NoUpdate)?;
        if transition.event.is_some() {
            return Err(ViewError::UnconsumedEvents);
        }
        self.current = transition.next;
        self.transition = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(x: i32, z: i32) -> TrackingView {
        TrackingView::positioned(ChunkAddress { x, z }, ViewDistance::server(32)).unwrap()
    }

    #[test]
    fn visited_cells_are_bounded_even_for_extreme_teleports_and_corner_overlap() {
        for (next, expected) in [
            (view(66, 66), 133 * 133),
            (view(100_000_000, -100_000_000), 2 * 67 * 67),
        ] {
            let mut difference = ViewDifference::new(view(0, 0), next);
            difference.by_ref().for_each(drop);
            assert_eq!(difference.visited, expected);
            assert_eq!(difference.next(), None);
            assert_eq!(difference.visited, expected);
        }
        let mut equal = ViewDifference::new(view(0, 0), view(0, 0));
        assert_eq!(equal.next(), None);
        assert_eq!(equal.visited, 0);
    }
}
